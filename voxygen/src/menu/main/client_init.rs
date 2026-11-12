use client::{
    Client, ClientInitStage, CredentialProvider, Credentials, ServerInfo,
    addr::ConnectionArgs,
    error::{Error as ClientError, NetworkConnectError, NetworkError},
    oauth::{OAuthLogin, OAuthProvider},
};
use common_net::msg::ClientType;
use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::runtime;
use tracing::{trace, warn};

#[derive(Debug)]
#[expect(clippy::enum_variant_names)] //TODO: evaluate ClientError ends with Enum name
pub enum Error {
    ClientError {
        error: ClientError,
        mismatched_server_info: Option<ServerInfo>,
    },
    ClientCrashed,
    ServerNotFound,
}

#[expect(clippy::large_enum_variant)]
pub enum Msg {
    IsAuthTrusted(String),
    /// The account has 2FA enabled and the password was correct -- the
    /// background task is now blocked on `code_rx.recv()`, waiting for
    /// `ClientInit::submit_2fa_code`.
    TwoFaRequired,
    /// A native OAuth attempt has opened the system browser and is now waiting
    /// for the player to finish there. `provider` is only for the wording of
    /// the waiting message.
    OAuthPending {
        provider: OAuthProvider,
    },
    /// The provider account isn't linked to any Xindeler account yet -- the
    /// background task is blocked on `oauth_username_rx.recv()`, waiting for
    /// `ClientInit::submit_oauth_username`.
    OAuthUsernameRequired {
        suggested: String,
    },
    Done(Result<Client, Error>),
}

pub struct AuthTrust(String, bool);

/// `CredentialProvider` for the main menu as it is today: the menu still
/// collects the username/password -- or the OAuth provider choice -- *before*
/// connecting, so this provider simply hands out what it was built with once
/// the server has told the client which auth server it uses. The trust
/// prompt and the 2FA prompt go through the same `Msg`s and channels the
/// menu already handles, so the player-facing flow is unchanged.
///
/// Built fresh for every connection attempt (see `ClientInit::new`).
struct MenuCredentialProvider {
    username: String,
    password: String,
    /// Taken by the first `get_credentials` call: `OAuthLogin` owns one-shot
    /// UI callbacks and there is exactly one OAuth run per attempt.
    oauth: Mutex<Option<OAuthLogin>>,
    tx: Sender<Msg>,
    trust_rx: Receiver<AuthTrust>,
    code_rx: Receiver<String>,
}

impl CredentialProvider for MenuCredentialProvider {
    fn auth_trusted(&self, auth_provider: &Option<String>) -> bool {
        // A server without an auth server has nothing to trust.
        let Some(auth_server) = auth_provider else {
            return true;
        };
        let _ = self.tx.send(Msg::IsAuthTrusted(auth_server.clone()));
        self.trust_rx
            .recv()
            .map(|AuthTrust(server, trust)| trust && server == *auth_server)
            .unwrap_or(false)
    }

    async fn get_credentials(&self, _auth_provider: &Option<String>) -> Result<Credentials, ()> {
        let oauth = self.oauth.lock().ok().and_then(|mut oauth| oauth.take());
        Ok(match oauth {
            Some(login) => Credentials::OAuth(login),
            None => Credentials::UsernamePassword(self.username.clone(), self.password.clone()),
        })
    }

    async fn get_two_fa_code(&self, _auth_provider: &Option<String>) -> Option<String> {
        // `code_rx.recv()` blocks until the player submits a code (or the
        // `ClientInit` is dropped, which drops `code_tx` and ends it with a
        // clean `Err` = cancel), so it runs on the blocking pool rather than
        // stalling the reactor. Crossbeam channels are MPMC and cheap to
        // clone.
        let tx = self.tx.clone();
        let code_rx = self.code_rx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::TwoFaRequired);
            code_rx.recv().ok()
        })
        .await
        .ok()
        .flatten()
    }
}

// Used to asynchronously parse the server address, resolve host names,
// and create the client (which involves establishing a connection to the
// server).
pub struct ClientInit {
    rx: Receiver<Msg>,
    stage_rx: Receiver<ClientInitStage>,
    trust_tx: Sender<AuthTrust>,
    code_tx: Sender<String>,
    oauth_username_tx: Sender<String>,
    cancel: Arc<AtomicBool>,
}
impl ClientInit {
    pub fn new(
        connection_args: ConnectionArgs,
        username: String,
        password: String,
        runtime: Arc<runtime::Runtime>,
        locale: Option<String>,
        config_dir: &Path,
        client_type: ClientType,
        oauth: Option<OAuthProvider>,
    ) -> Self {
        let (tx, rx) = unbounded();
        let (trust_tx, trust_rx) = unbounded();
        let (code_tx, code_rx) = unbounded();
        let (oauth_username_tx, oauth_username_rx) = unbounded();
        let (init_stage_tx, init_stage_rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel2 = Arc::clone(&cancel);

        let runtime2 = Arc::clone(&runtime);
        let config_dir = config_dir.to_path_buf();

        runtime.spawn(async move {
            let mut last_err = None;

            const FOUR_MINUTES_RETRIES: u64 = 48;
            'tries: for _ in 0..FOUR_MINUTES_RETRIES {
                if cancel2.load(Ordering::Relaxed) {
                    break;
                }
                let mismatched_server_info: Arc<Mutex<Option<ServerInfo>>> = Default::default();
                // Rebuilt per attempt: the `OAuthLogin` callbacks are one-shot
                // boxed closures that must own their captures to be `Send +
                // 'static` inside `spawn_blocking`. `cancel2` is the very
                // `Arc<AtomicBool>` that `Drop for ClientInit` sets, so
                // cancelling an OAuth attempt is the existing cancel-by-drop
                // idiom with nothing new bolted on.
                let oauth_login = oauth.map(|provider| {
                    let opened_tx = tx.clone();
                    let username_tx = tx.clone();
                    let username_rx = oauth_username_rx.clone();
                    let cancel3 = Arc::clone(&cancel2);
                    OAuthLogin {
                        provider,
                        cancelled: Box::new(move || cancel3.load(Ordering::Relaxed)),
                        on_browser_opened: Box::new(move || {
                            let _ = opened_tx.send(Msg::OAuthPending { provider });
                        }),
                        pick_username: Box::new(move |suggested| {
                            let _ = username_tx.send(Msg::OAuthUsernameRequired { suggested });
                            username_rx.recv().ok()
                        }),
                    }
                });
                let credential_provider = MenuCredentialProvider {
                    username: username.clone(),
                    password: password.clone(),
                    oauth: Mutex::new(oauth_login),
                    tx: tx.clone(),
                    trust_rx: trust_rx.clone(),
                    code_rx: code_rx.clone(),
                };

                match Client::new(
                    connection_args.clone(),
                    Arc::clone(&runtime2),
                    locale.clone(),
                    &credential_provider,
                    &|stage| {
                        if let ClientInitStage::ServerVersionArrived(ref info) = stage {
                            *mismatched_server_info.lock().unwrap() = Some(info.clone());
                        }
                        let _ = init_stage_tx.send(stage);
                    },
                    crate::ecs::sys::add_local_systems,
                    config_dir.clone(),
                    client_type,
                )
                .await
                {
                    Ok(client) => {
                        let _ = tx.send(Msg::Done(Ok(client)));
                        // `block_in_place` only permits blocking I/O on this
                        // worker thread -- it does not make it safe to drop
                        // the very `Runtime` this task is running on (tokio
                        // still panics: "Cannot drop a runtime in a context
                        // where blocking is not allowed"). A real OS thread,
                        // outside any tokio context, is the documented-safe
                        // way to run that final drop.
                        std::thread::spawn(move || drop(runtime2));
                        return;
                    },
                    Err(ClientError::NetworkErr(NetworkError::ConnectFailed(
                        NetworkConnectError::Io(e),
                    ))) => {
                        warn!(?e, "Failed to connect to the server. Retrying...");
                    },
                    Err(e) => {
                        trace!(?e, "Aborting server connection attempt");
                        let mismatched_server_info = mismatched_server_info.lock().unwrap().take();
                        last_err = Some(Error::ClientError {
                            error: e,
                            mismatched_server_info,
                        });
                        break 'tries;
                    },
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }

            // Parsing/host name resolution successful but no connection
            // succeeded If last_err is None this typically means
            // there was no server up at the input address and all
            // the attempts timed out.
            let _ = tx.send(Msg::Done(Err(last_err.unwrap_or(Error::ServerNotFound))));

            // Safe drop runtime -- see the comment on the identical call
            // above (the `Ok(client)` branch) for why this must be a real
            // OS thread rather than `block_in_place`.
            std::thread::spawn(move || drop(runtime2));
        });

        ClientInit {
            rx,
            stage_rx: init_stage_rx,
            trust_tx,
            code_tx,
            oauth_username_tx,
            cancel,
        }
    }

    /// Poll if the thread is complete.
    /// Returns None if the thread is still running, otherwise returns the
    /// Result of client creation.
    pub fn poll(&self) -> Option<Msg> {
        match self.rx.try_recv() {
            Ok(msg) => Some(msg),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Msg::Done(Err(Error::ClientCrashed))),
        }
    }

    /// Poll for connection stage updates from the client
    pub fn stage_update(&self) -> Option<ClientInitStage> { self.stage_rx.try_recv().ok() }

    /// Report trust status of auth server
    pub fn auth_trust(&self, auth_server: String, trusted: bool) {
        let _ = self.trust_tx.send(AuthTrust(auth_server, trusted));
    }

    /// Answers a pending `Msg::TwoFaRequired` with the code the player
    /// entered. There is no cancel-specific method here -- like any other
    /// mid-connect abort, cancelling the 2FA prompt drops this whole
    /// `ClientInit` (see `Drop`/`cancel`), which drops `code_tx` and
    /// unblocks the background task's `code_rx.recv()` with a clean `Err`.
    pub fn submit_2fa_code(&self, code: String) { let _ = self.code_tx.send(code); }

    /// Answers a pending `Msg::OAuthUsernameRequired` with the username the
    /// player chose. Like `submit_2fa_code`, there is no cancel-specific
    /// method: cancelling drops this whole `ClientInit`, which drops
    /// `oauth_username_tx` and unblocks the background task's `recv()`.
    pub fn submit_oauth_username(&self, username: String) {
        let _ = self.oauth_username_tx.send(username);
    }

    pub fn cancel(&mut self) { self.cancel.store(true, Ordering::Relaxed); }
}

impl Drop for ClientInit {
    fn drop(&mut self) { self.cancel(); }
}
