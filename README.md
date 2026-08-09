> [!IMPORTANT]
> ## ⚠️ DISCONTINUED — this codebase has been superseded
> **As of 2026-07-02, active development of Xindeler moved to a new architecture built on the
> [Bevy](https://bevy.org) engine, at [`Matute289/xindeler`](https://github.com/Matute289/xindeler).**
>
> This repository (`xindeler-old`) contains the previous engine generation (Veloren's bespoke
> wgpu/specs stack) and is kept as **historical reference** and as the **interim playable build**
> until the Bevy client reaches feature parity. Only critical fixes land here — no new features.

<!-- SPDX-SnippetBegin -->
<!-- SPDX-SnippetCopyrightText: 2025 Hrom -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# ![Xindeler banner](https://cdn.xindeler.greenmountain.dev/images/xindeler-banner.webp)
<!-- SPDX-SnippetEnd -->

## Welcome to Xindeler!

Xindeler is an open-source MMORPG built in Rust, originally forked from Veloren and evolving into its own persistent online world focused on exploration, combat, crafting and magic.

Inspired by classic sandbox RPGs and living virtual worlds, Xindeler aims to provide a rich multiplayer experience where players shape the future of the world through their actions, discoveries, and interactions.

Xindeler is in active development.

## Useful Links

### Account Management

[Sign Up](https://auth.xindeler.greenmountain.dev)

Create an account to access Xindeler services and future multiplayer features.

### Community Wiki

[Wiki](https://wiki.xindeler.greenmountain.dev)

Community-driven information repository about the world, mechanics, lore, crafting, creatures, and gameplay systems.

### Documentation

[Documentation](https://docs.xindeler.greenmountain.dev)

Technical and gameplay documentation for players, contributors, developers, and server administrators.

### Downloads

[Downloads](https://downloads.xindeler.greenmountain.dev)

Official game downloads and launcher distribution.

## Official Services

| Service        | URL                                          |
| -------------- | -------------------------------------------- |
| Website        | https://xindeler.greenmountain.dev           |
| Authentication | https://auth.xindeler.greenmountain.dev      |
| Wiki           | https://wiki.xindeler.greenmountain.dev      |
| Documentation  | https://docs.xindeler.greenmountain.dev      |
| Downloads      | https://downloads.xindeler.greenmountain.dev |
| CDN            | https://cdn.xindeler.greenmountain.dev       |

## Getting Xindeler

Official builds for Windows, macOS and Linux will be available through the Downloads portal.

Due to active development, builds may change frequently and compatibility between versions is not guaranteed.

It is recommended to use Airshipper, the current launcher solution inherited from the Veloren ecosystem, to simplify updates and installation. Future launcher solutions may be introduced as the project evolves.

If you prefer to compile the game yourself, please refer to the project documentation.

### A note on building from source

Xindeler authenticates players against its own account service, and the client
library for that service lives in a private repository. Building this project
therefore requires read access to that repository, which is granted to the
Xindeler team.

If you clone this repository without that access, `cargo` will fail to fetch the
`authc` dependency and the build will not start. Everything else in the tree
builds normally; only the account integration is gated. Adapting the project to
a different account service, or stubbing it out, is left to you.


## Contributing

Xindeler welcomes contributions from developers, artists, writers, translators, content creators, and community members.

Areas of contribution include:

* Gameplay systems
* Multiplayer infrastructure
* AI-driven NPC systems
* World generation
* Quests and narrative content
* Art and animations
* Sound and music
* Documentation
* Localization

Please consult the documentation site for contribution guidelines and development setup instructions.

## Roadmap

Planned features include:

* Persistent MMORPG infrastructure
* Expanded crafting systems
* Magic and spellcasting systems
* AI-powered NPC interactions
* Dynamic quest generation
* Living settlements and factions
* Enhanced multiplayer experiences
* Expanded world-building and lore

## FAQ

### Q: Is Xindeler free to play?

**A:** Yes. Xindeler is free to play and open source.

### Q: Is Xindeler open source?

**A:** Yes. Xindeler is developed as an open-source project and welcomes community contributions.

### Q: What platforms are supported?

**A:** Xindeler targets Windows, macOS and Linux systems. Additional platforms may be supported in the future.

### Q: Is Xindeler related to Veloren?

**A:** Xindeler originated as a fork of the Veloren project and continues to build upon that foundation while pursuing its own vision, systems, infrastructure, and gameplay direction.

## License

Xindeler remains an open-source project. Refer to the repository license files for detailed licensing information.

## Status

🚧 Active Development

The project is currently undergoing infrastructure migration, rebranding, and feature expansion as part of its evolution from its original Veloren-based foundation into a standalone MMORPG platform.
