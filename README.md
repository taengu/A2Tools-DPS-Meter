# A2Tools DPS Meter

[![License](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](LICENSE)
[![GitHub Issues](https://img.shields.io/github/issues/taengu/A2Tools-DPS-Meter)](https://github.com/taengu/A2Tools-DPS-Meter/issues)
[![GitHub Pull Requests](https://img.shields.io/github/issues-pr/taengu/A2Tools-DPS-Meter)](https://github.com/taengu/A2Tools-DPS-Meter/pulls)

Real-time DPS meter overlay for AION 2. Captures game network packets to display damage, skills, and combat statistics.

The **[AION 2 DPS Meter website, a2tools.app](https://a2tools.app)** has the download, shared fight logs, leaderboards and class stats.

**[Download Latest Release](https://github.com/taengu/A2Tools-DPS-Meter/releases)** | **[A2Tools.app](https://a2tools.app)**

[한국어](README_KO.md) | [简体中文](README_ZH.md) | [繁體中文](README_ZH-TW.md)

## Features

- Real-time DPS tracking with per-player breakdown
- Skill-level damage analysis with crit, back attack, parry, double, and perfect rates, plus optional block, perfect block, endurance, regeneration, miss and resist columns
- DOT (damage over time) tracking
- Summon damage merged with owner
- Multiple target selection modes (Boss, Last Hit, All Targets, Train)
- DPS chart and timeline visualization
- Battle history with auto-save for boss fights
- Ping monitoring
- Ten languages: English, Korean, Chinese (Traditional and Simplified), Japanese, German, French, Spanish, Portuguese and Russian
- Always-on-top transparent overlay
- Themes and customization
- Stream overlay for OBS on another PC: add the meter as a Browser Source, and choose the name shown for you
- Fight uploads to [a2tools.app](https://a2tools.app/logs): a link to share, leaderboards and class stats
- Windows and Linux ([Linux guide](docs/linux.md))

## Requirements

**Windows**

- **Windows 10/11** (x86_64)
- **[Npcap](https://npcap.com)** — required for packet capture
  - During Npcap installation, check **"Install Npcap in WinPcap API-compatible Mode"**
- **Administrator privileges** — required for raw packet capture

**Linux** (playing through Proton)

- **64-bit (x86_64) with WebKitGTK 4.1:** Ubuntu 22.04 or newer, Debian 12, Fedora 39 or newer, openSUSE, current Arch, CachyOS, Manjaro or EndeavourOS, Bazzite and other image-based Fedoras, or Steam Deck (SteamOS, through distrobox)
- Packages and setup for each: the **[Linux guide](docs/linux.md)**

## Installation

1. Install [Npcap](https://npcap.com) with WinPcap API-compatible mode enabled
2. Download the latest MSI installer from [Releases](https://github.com/taengu/A2Tools-DPS-Meter/releases)
3. Run the installer
4. Launch A2Tools DPS Meter (run as Administrator)

**Linux:** packages for Ubuntu/Debian (.deb), Fedora/openSUSE (.rpm), Bazzite, Arch/CachyOS/Manjaro and Steam Deck — see the **[Linux guide](docs/linux.md)**.

[![Ubuntu](https://img.shields.io/badge/Ubuntu-E95420?logo=ubuntu&logoColor=white)](docs/linux.md#ubuntu-debian-linux-mint-pop_os) [![Debian](https://img.shields.io/badge/Debian-A81D33?logo=debian&logoColor=white)](docs/linux.md#ubuntu-debian-linux-mint-pop_os) [![Linux Mint](https://img.shields.io/badge/Linux_Mint-87CF3E?logo=linuxmint&logoColor=white)](docs/linux.md#ubuntu-debian-linux-mint-pop_os) [![Pop!_OS](https://img.shields.io/badge/Pop%21__OS-48B9C7?logo=popos&logoColor=white)](docs/linux.md#ubuntu-debian-linux-mint-pop_os) [![Fedora](https://img.shields.io/badge/Fedora-51A2DA?logo=fedora&logoColor=white)](docs/linux.md#fedora) [![Bazzite](https://img.shields.io/badge/Bazzite-8A3FFC?logo=fedora&logoColor=white)](docs/linux.md#bazzite-silverblue-kinoite-aurora-bluefin) [![Steam Deck](https://img.shields.io/badge/Steam_Deck-1A9FFF?logo=steamdeck&logoColor=white)](docs/linux.md#steam-deck-steamos) [![openSUSE](https://img.shields.io/badge/openSUSE-73BA25?logo=opensuse&logoColor=white)](docs/linux.md#opensuse) [![Arch](https://img.shields.io/badge/Arch-1793D1?logo=archlinux&logoColor=white)](docs/linux.md#cachyos-arch-manjaro-endeavouros) [![CachyOS](https://img.shields.io/badge/CachyOS-08A88A?logo=cachyos&logoColor=white)](docs/linux.md#cachyos-arch-manjaro-endeavouros) [![Manjaro](https://img.shields.io/badge/Manjaro-35BF5C?logo=manjaro&logoColor=white)](docs/linux.md#cachyos-arch-manjaro-endeavouros) [![EndeavourOS](https://img.shields.io/badge/EndeavourOS-7F3FBF?logo=endeavouros&logoColor=white)](docs/linux.md#cachyos-arch-manjaro-endeavouros)

## Streaming from another PC (OBS)

If OBS runs on a second PC, it cannot capture the meter's window on the game PC. The meter can serve its display over your home network instead:

1. On the game PC, open **Settings → Streaming** and turn on **Stream overlay (OBS on another PC)**. The default port is 18731; change it if something else uses it.
2. The first time, Windows Defender Firewall asks whether to allow the meter. Allow it on **Private networks** (your home network). The meter never adds a firewall rule by itself.
3. Copy the URL shown (`http://<game PC's LAN IP>:18731/overlay?key=<key>`). If the PC has more than one network address, each is listed; use the one on the same network as the streaming PC.
4. In OBS on the streaming PC, add a **Browser Source** with that URL. The background is transparent, and the rows scale with the source's width.

The overlay shows the boss, the fight timer and the rows the meter shows (name, class, DPS, damage, share), and updates a few times a second. It is read-only and only reachable with the key; **New key** cuts off every link handed out before. Add `&lang=ko` (or `en`, `de`, `ja`, …) to the URL to change the language of its few labels. Add `&rows=5` to show at most five rows, so they always fit the height you gave the source (about 0.43 × its width for five rows, plus 0.07 × width for each more); you stay on it, in the last row if you are below the cut. **Your name on stream** sets the name the overlay shows for you; leave it blank to show your character name. Turn the setting off and the meter stops listening.

## Building from Source

### Prerequisites

- [Rust](https://rustup.rs/) (latest stable)
- [Node.js](https://nodejs.org/) (v18+)
- [Npcap](https://npcap.com) installed

### Build

```bash
npm install
npm run tauri build
```

The MSI installer will be at `src-tauri/target/release/bundle/msi/`.

### Development

```bash
npm run tauri dev
```

### Building a private (offline) copy

For anyone who wants a meter that talks to nothing but the game: build it yourself without its online features.

```bash
npm install
npm run build:offline
```

That runs `tauri build -- --no-default-features --features desktop`, which leaves out the Cargo feature `online`. The code below is not switched off in this build, it is not in it: the network client (`reqwest`) and the Discord client are not linked at all, which `cargo tree --no-default-features --features desktop` shows.

Left out:

- the A2 Tools account, uploading logs to a2tools.app, and automatic uploads
- the supporter roster download (gold names); a local `patrons-local.bin` still works
- update checks and the updater
- Send Logs to Dev
- Discord activity ("what I'm playing")
- the stream overlay for OBS (it serves the meter on your network)
- downloading Npcap's installer: install [Npcap](https://npcap.com/#download) yourself first

Settings and History hide what is not there. Everything else works the same, offline: capture, the meter, Details, History, buffs, screenshots, the upload preview (it writes what an upload would contain, to disk only).

Two things still leave the computer, and only these: skill and buff icons, loaded from the game's own CDN (`assets.playnccdn.com`; the window's content security policy allows nothing else), and links you click yourself (Discord, Support), which open in your browser. `npm install` and the Rust build fetch their packages as usual. Run it without building with `npm run dev:offline`.

## FAQ

**Q: The meter shows "Detecting AION2 connection..."**
A: Make sure AION 2 is running and the app has administrator privileges. If using a VPN or ping reducer, the app will detect the loopback adapter automatically.

**Q: My name doesn't appear on the meter**
A: Enter your character name and actor ID in Settings. The name is auto-detected from the AION 2 window title.

**Q: Npcap is installed but capture doesn't work**
A: Reinstall Npcap and ensure "WinPcap API-compatible Mode" is checked during installation.

## Community

- [Discord](https://discord.gg/Aion2Global)
- [A2Tools.app](https://a2tools.app)

### Building your own meter?

Your meter can upload its logs to [a2tools.app](https://a2tools.app/logs) as well. The site stores them, gives each one a link to share, and puts them on the leaderboard and in its stats. To connect yours, join the [Discord](https://discord.gg/Aion2Global) and post in **#aion2-dpsmeter**.

## Support

Say thanks and fund new cool projects & features!

- <img src="wechat.png" width="150">
- ☕ [Buy me a Coffee](https://ko-fi.com/hiddencube)
- ☕ [在爱发电支持我](https://afdian.com/a/hiddencube)
- 🅿️ [Send with PayPal](https://www.paypal.me/taengoo)
- 🎁 [Donate with Crypto](https://nowpayments.io/donation/thehiddencube)
- **BTC**: `1GexKhgVZPYRqpfCKydXLoNUXRRRUoAUwT`
- **ETH**: `0x38F0bc371A563A24eCa6034cFf77eB6173c7e3e7`
- **USDC**: `0xA9571Fc95666350f6DFFB8Fb80ee27eE7db46b56`

## License

[GPL-3.0](LICENSE)

### Fixes from Daevalog

Some parser and storage fixes come from [Daevalog](https://github.com/Seralth/Daevalog), Seralth's GPL-3.0 fork of this meter. Each one is committed under Seralth's name, with a `Ported-from:` line naming the original commit. The buff and debuff parser (`src-tauri/src/capture/abnormal.rs`) and its data tables (`src/data/abnormals.json`, `src/data/i18n/abnormals/`) come from there too.

### Fonts

The meter comes with these fonts. Each is under the SIL Open Font License, version 1.1.

- Noto Sans SC: [public/vendor/fonts/noto-sans-sc/LICENSE](public/vendor/fonts/noto-sans-sc/LICENSE)
- Noto Sans TC: [public/vendor/fonts/noto-sans-tc/LICENSE](public/vendor/fonts/noto-sans-tc/LICENSE)
- Pretendard: [public/vendor/fonts/pretendard/LICENSE](public/vendor/fonts/pretendard/LICENSE)
