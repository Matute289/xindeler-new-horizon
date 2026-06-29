---
name: xindeler-ai-npc
description: Use when building the generative-AI NPC layer for AURORA — automating NPC persona/dialogue/voice generation (offline, baked) and the optional live STT→LLM→TTS conversation tier. Covers authoring agents, ElevenLabs MCP, self-hosted faster-whisper/LLaMA, the offline-bake-vs-live-tier architecture, and the AURORA invariants. For the core social-sim use xindeler-aurora.
---

# xindeler-ai-npc

The generative-AI NPC layer of **AURORA**. Design: `docs/design/specs/2026-06-24-aurora-generative-npc-design.md`
(companion of the core `2026-06-10-project-aurora-design.md`); tasks `tasks/23`. Pair with
**xindeler-aurora** (the rtsim social sim) and delegate persona writing to the **`npc-persona-writer`** agent.

## Non-negotiables (inherited from AURORA — never break)
1. **No LLM in the tick path.** Tier 1 is fully baked; Tier 2 is async/off-tick, cached, with a
   deterministic **template fallback**.
2. **Server-authoritative.** All NPC state in `rtsim`; clients get dialogue/voice/behavior only.
3. **Runtime self-hosted / no external dependency for the live game.** External authoring agents/APIs,
   ElevenLabs) are used **only at author-time** (offline bake). Live tier (if built) self-hosts on the VPS.
4. **Anything *I* run goes through an MCP or API** (ElevenLabs MCP is connected; self-host exposes HTTP).
5. **Lore-grounded + moderated** — RAG over `docs/design/lore/`; reuse the canon denylist; review baked
   output. No lore-breaking hallucinations (Inworld's safety-rail pattern).

## Two tiers
- **Tier 1 — offline pipeline (v1, recommended):** Codex or Claude + ElevenLabs (MCP)
  pre-generate **personas, dialogue, rumors, baked voice clips** → committed as game data/assets → the
  live server reads baked data only. This *is* the pragmatic NPC-generation automation.
- **Tier 2 — live conversation (post-v1, opt-in):** self-host **faster-whisper** (STT) + **LLaMA** (LLM)
  + a fast **TTS** (Cartesia/Deepgram or self-host Fish/Coqui) on the VPS; async, cached, latency-budgeted.

## Tool choices (research-backed — see the spec §4/§8)
- **LLM gen:** an authoring agent (offline) · self-host LLaMA (live). **STT:** faster-whisper (self-host, Docker).
- **TTS:** **ElevenLabs via MCP** for baked voice (latency irrelevant for batch); live = Cartesia/
  Deepgram/Fish. **❌ PlayHT** — Meta-acquired, winding down; do not adopt.
- **Faces:** voxel mouth-flap/emote sync, **not** NVIDIA Audio2Face (heavy/NVIDIA-bound).

## Offline bake workflow (Tier 1)
1. `npc-persona-writer` agent → persona + dialogue pack from a role/faction + lore (RAG), within the
   persona schema's ethical bounds.
2. Moderation pass (filters + canon denylist + review).
3. Bake dialogue/persona into game data (assets vs rtsim — per spec §9 decision); wire into AURORA's
   `DialogueKind` (`rtsim/src/rule/npc_ai/dialogue.rs`).
4. Voice: ElevenLabs MCP (`generate_audio`/`list_voices`/`create_voice`) → per-NPC clips → assets
   (binary → **VPS-LFS**). Add a `voxygen` audio path + mouth/emote sync.

## Where it lives in code (with xindeler-aurora)
NPC persona/memory: `rtsim/src/data/npc.rs`, `common/src/rtsim.rs`. Dialogue: `common/src/rtsim.rs`
(`DialogueKind`) + `rtsim/src/rule/npc_ai/dialogue.rs`. Voice/audio: `voxygen/src/audio/`. RAG corpus:
`docs/design/lore/`.

## Reviews
`sim-systems-engineer` (rtsim), `game-architecture-reviewer` (content-as-data, not code), `xindeler-review`.
In-game smoke via `xindeler-run`.
