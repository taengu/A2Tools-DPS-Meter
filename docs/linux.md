# A2Tools DPS Meter on Linux (Proton)

The meter runs natively on Linux while AION 2 runs under Proton. Install the package for your system below: it sets up everything, packet-capture permission included, and keeps itself up to date. Only on a distribution with no package do you need to build it yourself. Linux support is new, so your logs help: see [Sending us your logs](#sending-us-your-logs).

Every package needs a 64-bit (x86_64) system with WebKitGTK 4.1: Ubuntu 22.04, Debian 12, Fedora 39 or newer, any current Arch, Bazzite, or SteamOS through distrobox.

## Contents

[![Ubuntu](https://img.shields.io/badge/Ubuntu-E95420?logo=ubuntu&logoColor=white)](#ubuntu-debian-linux-mint-pop_os) [![Debian](https://img.shields.io/badge/Debian-A81D33?logo=debian&logoColor=white)](#ubuntu-debian-linux-mint-pop_os) [![Linux Mint](https://img.shields.io/badge/Linux_Mint-87CF3E?logo=linuxmint&logoColor=white)](#ubuntu-debian-linux-mint-pop_os) [![Pop!_OS](https://img.shields.io/badge/Pop%21__OS-48B9C7?logo=popos&logoColor=white)](#ubuntu-debian-linux-mint-pop_os) [![Fedora](https://img.shields.io/badge/Fedora-51A2DA?logo=fedora&logoColor=white)](#fedora) [![Bazzite](https://img.shields.io/badge/Bazzite-8A3FFC?logo=fedora&logoColor=white)](#bazzite-silverblue-kinoite-aurora-bluefin) [![Steam Deck](https://img.shields.io/badge/Steam_Deck-1A9FFF?logo=steamdeck&logoColor=white)](#steam-deck-and-steam-machine-steamos) [![openSUSE](https://img.shields.io/badge/openSUSE-73BA25?logo=opensuse&logoColor=white)](#opensuse) [![Arch](https://img.shields.io/badge/Arch-1793D1?logo=archlinux&logoColor=white)](#cachyos-arch-manjaro-endeavouros) [![CachyOS](https://img.shields.io/badge/CachyOS-08A88A?logo=cachyos&logoColor=white)](#cachyos-arch-manjaro-endeavouros) [![Manjaro](https://img.shields.io/badge/Manjaro-35BF5C?logo=manjaro&logoColor=white)](#cachyos-arch-manjaro-endeavouros) [![EndeavourOS](https://img.shields.io/badge/EndeavourOS-7F3FBF?logo=endeavouros&logoColor=white)](#cachyos-arch-manjaro-endeavouros)

- **[Install](#install):** [Ubuntu, Debian, Mint, Pop!_OS](#ubuntu-debian-linux-mint-pop_os) · [Fedora](#fedora) · [Bazzite and other image-based Fedoras](#bazzite-silverblue-kinoite-aurora-bluefin) · [Steam Deck and Steam Machine (SteamOS)](#steam-deck-and-steam-machine-steamos) · [openSUSE](#opensuse) · [Arch, CachyOS, Manjaro, EndeavourOS](#cachyos-arch-manjaro-endeavouros)
- **[Update](#update):** [how each install updates](#how-each-install-updates) · [the update prompt](#the-update-prompt) · [by hand](#update-by-hand) · [which version do I have?](#which-version-do-i-have)
- **[Start and remove](#start-and-remove)**
- **[What works on Linux](#what-works-on-linux)**
- **[GNOME: keep the meter above other windows](#gnome-keep-the-meter-above-other-windows)**
- **[Build from source](#build-from-source-other-distributions)**, for distributions with no package
- **[Sending us your logs](#sending-us-your-logs)**
- **[Tiling desktops](#tiling-desktops-hyprland-sway-i3)** (Hyprland, Sway, i3)
- **[Troubleshooting](#troubleshooting)**

## Install

Each package grants the packet-capture permission itself, so you never need `setcap`.

### Ubuntu, Debian, Linux Mint, Pop!_OS

```bash
curl -LO https://cdn.a2tools.app/linux/a2tools-dps-meter-latest_amd64.deb
```

```bash
sudo apt install ./a2tools-dps-meter-latest_amd64.deb
```

### Fedora

Add the A2Tools repository once, then install from it. Updates then arrive with your normal system updates.

```bash
sudo curl -Lo /etc/yum.repos.d/a2tools.repo https://cdn.a2tools.app/linux/a2tools.repo
```

```bash
sudo dnf install a2-tools-dps-meter
```

The repository and its packages are signed. dnf asks once whether to trust the A2Tools key; its fingerprint is `C955 98B7 90B4 DBE0 3F42 A314 50A5 2827 76C1 84DA`. If you added the repository before version 2.0.87, run the `curl` line again so your system checks the signatures too. This applies to the image-based Fedoras below as well.

### Bazzite, Silverblue, Kinoite, Aurora, Bluefin

These image-based Fedoras keep the system read-only and add packages by layering them with `rpm-ostree`, which takes effect after a restart. Add the A2Tools repository, layer the meter, and restart:

```bash
sudo curl -Lo /etc/yum.repos.d/a2tools.repo https://cdn.a2tools.app/linux/a2tools.repo
```

```bash
rpm-ostree install a2-tools-dps-meter
```

```bash
systemctl reboot
```

Updates then come with your system updates: Bazzite installs them on its own, or run `rpm-ostree upgrade` and restart. On a Steam Deck running Bazzite, use **Desktop Mode**: in Game Mode, nothing can draw over the game.

#### Updating without a restart (advanced)

`rpm-ostree` can also apply a meter update to the running system. Close the meter, then:

```bash
rpm-ostree upgrade
```

```bash
sudo rpm-ostree apply-live --allow-replacement
```

Start the meter again and check the version with `rpm -q a2-tools-dps-meter`.

- `rpm-ostree upgrade` also fetches any new Bazzite system image waiting at the time, and `apply-live` would then apply that system update live too, which is best avoided. If one is waiting (`rpm-ostree status` shows a new base image), restart once for it, and use this only when the meter alone has changed.
- `--allow-replacement` is needed because the meter is already installed: without it, `apply-live` only adds new packages.
- If anything misbehaves afterwards, including packet capture, a normal restart sets it right: the update is staged either way.

### Steam Deck and Steam Machine (SteamOS)

SteamOS replaces its read-only system with every update, so anything installed into it directly is wiped. Instead, the meter goes in a **distrobox**: a container with its own Arch Linux inside, which SteamOS 3.5 and later include. It must be created with `--root`: an ordinary (rootless) container cannot read the game's network traffic.

The overlay only works in **Desktop Mode**. In Game Mode, nothing can draw over the game.

1. Switch to Desktop Mode: press the **Steam** button, then **Power**, then **Switch to Desktop**.
2. Open **Konsole** from the application menu. If you have never set a password for the `deck` user, set one now (sudo needs it):

    ```bash
    passwd
    ```

3. Keep the box on your home partition. SteamOS stores containers made with `--root` on a small system partition, which the box doesn't fit on ("insufficient space"). Point that storage at your home partition, once:

    ```bash
    sudo mkdir -p /home/.a2tools-containers
    ```

    ```bash
    sudo tee /etc/containers/storage.conf <<'EOF'
    [storage]
    driver = "overlay"
    graphroot = "/home/.a2tools-containers"
    runroot = "/run/containers/storage"
    EOF
    ```

    If an earlier attempt stopped with "insufficient space", clear what it left behind first: `sudo podman system reset --force`. SteamOS keeps this setting through its updates.

4. Create the box:

    ```bash
    distrobox create --root --name a2tools --image archlinux:latest
    ```

    The first time you enter it (the next step), it takes a few minutes to set up and asks you to choose a password for your user inside the box. Any password will do; sudo inside the box asks for it.

5. Download the meter and install it inside the box:

    ```bash
    curl -LO https://cdn.a2tools.app/linux/a2tools-dps-meter-latest-x86_64.pkg.tar.zst
    ```

    ```bash
    distrobox enter --root a2tools -- sudo pacman -Syu --noconfirm
    ```

    ```bash
    distrobox enter --root a2tools -- sudo pacman -U --noconfirm ~/a2tools-dps-meter-latest-x86_64.pkg.tar.zst
    ```

6. Start AION 2 from Steam, still in Desktop Mode, set to borderless or windowed. Then start the meter from Konsole:

    ```bash
    distrobox enter --root a2tools -- a2tools-dps-meter
    ```

SteamOS updates leave the box alone, so the meter survives them. This route is new and not yet confirmed on a real Steam Deck or Steam Machine: please tell us how it goes.

### openSUSE

```bash
curl -LO https://cdn.a2tools.app/linux/a2tools-dps-meter-latest.x86_64.rpm
```

```bash
sudo zypper install --allow-unsigned-rpm ./a2tools-dps-meter-latest.x86_64.rpm
```

### CachyOS, Arch, Manjaro, EndeavourOS

pacman refuses unsigned packages straight from a URL, hence the download first:

```bash
curl -LO https://cdn.a2tools.app/linux/a2tools-dps-meter-latest-x86_64.pkg.tar.zst
```

```bash
sudo pacman -U a2tools-dps-meter-latest-x86_64.pkg.tar.zst
```

## Update

### How each install updates

| Installed on | Updates |
| --- | --- |
| Ubuntu, Debian, Mint, Pop!_OS | The meter offers each new version: [the update prompt](#the-update-prompt) |
| Fedora | With your system updates (`sudo dnf upgrade`); the meter also offers them |
| Bazzite and other image-based Fedoras | With your system updates; the meter does not prompt |
| Steam Deck (SteamOS) | [By hand](#update-by-hand) |
| openSUSE | The meter offers each new version |
| Arch, CachyOS, Manjaro, EndeavourOS | The meter offers each new version |

### The update prompt

When a new version is out, the meter asks "A new update is available! … Download and install now?" shortly after it starts:

1. Click **Yes**. The meter downloads the update and closes.
2. Your desktop asks for your password, the same prompt as for other system changes. Enter it.
3. The meter starts again by itself, on the new version.

If you cancel the password prompt, the meter restarts on the old version and asks again next time.

### Update by hand

Run the [Install](#install) commands for your system again: the address always serves the newest version, and installing it over the old one keeps your settings and fight history. On a Steam Deck, that is the `curl` line and the last `pacman -U` line of step 4.

Do this once if your version cannot update itself: on Arch, `2.0.30.r70.g0ac3fb6-1`, the first test package.

### Which version do I have?

Ubuntu, Debian, Mint, Pop!_OS:

```bash
dpkg -s a2-tools-dps-meter | grep Version
```

Fedora, Bazzite and other image-based Fedoras, openSUSE:

```bash
rpm -q a2-tools-dps-meter
```

Arch, CachyOS, Manjaro, EndeavourOS:

```bash
pacman -Q a2tools-dps-meter
```

Steam Deck (SteamOS):

```bash
distrobox enter --root a2tools -- pacman -Q a2tools-dps-meter
```

## Start and remove

The meter is in your application menu, as A2Tools DPS Meter; on a Steam Deck, start it from Konsole as in [step 6](#steam-deck-and-steam-machine-steamos). To start it from a terminal with its output saved, which helps if you send us logs:

```bash
a2tools-dps-meter 2>&1 | tee ~/meter-console.log
```

To remove it:

| Installed on | Command |
| --- | --- |
| Ubuntu, Debian, Mint, Pop!_OS | `sudo apt remove a2-tools-dps-meter` |
| Fedora | `sudo dnf remove a2-tools-dps-meter` |
| Bazzite and other image-based Fedoras | `rpm-ostree uninstall a2-tools-dps-meter`, then restart |
| Steam Deck (SteamOS) | `distrobox rm --root a2tools` (removes the whole box) |
| openSUSE | `sudo zypper remove a2-tools-dps-meter` |
| Arch, CachyOS, Manjaro, EndeavourOS | `sudo pacman -R a2tools-dps-meter` |

## What works on Linux

| Feature | On Linux |
| --- | --- |
| Damage meter, Details, History | Works |
| Ping | Works |
| Finding the game | Looks for the running AION2.exe process under Proton |
| A2 Tools account sign-in | Works: kept in KWallet or GNOME Keyring, which may ask to create or unlock a wallet the first time |
| Automatic updates | Works for the packages; builds from source update with `git pull` |
| Class icons | Works (missing in the 2.0.34 package and earlier; fixed in 2.0.35) |
| Global hotkeys | From 2.0.59, once turned on in Settings → Keybinds, through your desktop's global shortcuts: see [Global hotkeys](#global-hotkeys) |
| Meter anchor (Top, Centre, Bottom) | Settings → **Meter stays fixed at**. Offered on KDE Plasma Wayland and when the meter runs through XWayland (the default on GNOME). Not offered where it would do nothing: in an X11 session, and on native Wayland outside KDE. Until you choose, the meter grows as before: from its top edge through XWayland, from its centre on KDE Plasma Wayland |
| Click-through lock | From 2.0.42, when the meter runs through XWayland (`GDK_BACKEND=x11 a2tools-dps-meter`): turn on the lock button in Settings. On native Wayland an app cannot see where the pointer is outside its own window, so the button stays hidden there |
| Discord activity | From 2.0.42: your class, level and server in your Discord status, through the Discord app on the same computer |
| Screenshots | Works from 2.0.41, to the clipboard and a folder (`~/Pictures/A2Tools DPS Meter` by default). On Linux the meter pictures itself on a plain background, since Wayland lets no app copy the screen |
| Auto-hide when the game loses focus | Not yet (the meter stays visible) |

## Global hotkeys

From 2.0.59 the meter's three hotkeys (reset, show or hide, click-through lock) work on Linux through your desktop's global shortcuts. They are off until you turn on **Use keybinds** under **Settings → Keybinds**; turning it on registers them at once, and turning it off releases them. Wayland lets no app grab keys itself, so the meter asks the desktop for them, and the desktop has the final say on which keys they use. The meter suggests the keys set on the same tab.

The keys are registered by a small helper, `a2tools-hotkeys`, installed next to the meter. The meter itself holds the permission to capture packets (`cap_net_raw`), and Linux will not let the desktop's shortcut service read a program holding a permission the service does not have, so the meter cannot register shortcuts itself. The helper has no such permission and only handles keybinds. They are off by default because your desktop may ask you to approve them.

| Desktop | What happens |
| --- | --- |
| KDE Plasma 6 (Bazzite, Kinoite, Aurora) | On the first start, Plasma asks you to confirm the meter's shortcuts. Change them later in **System Settings → Keyboard → Shortcuts** |
| GNOME 48 and later (Bluefin, Silverblue) | GNOME asks you to confirm the shortcuts on the first start; change them in **Settings → Keyboard → Keyboard Shortcuts** |
| Hyprland | The meter binds its keys itself each time it starts, unless a key is already bound in your config. To bind them yourself instead, see below |
| Other desktops | Works if the desktop's portal supports global shortcuts; otherwise there are no hotkeys |

**Hyprland.** The meter's shortcuts are `com.a2tools.dps-meter:reset`, `:toggle` and `:lock`. `hyprctl globalshortcuts` lists them while the meter runs. The meter's own bindings last until Hyprland reloads its config, and come back when the meter starts again. To bind them in your config instead:

```lua
hl.bind("CTRL + ALT + R", hl.dsp.global("com.a2tools.dps-meter:reset"))
hl.bind("CTRL + ALT + Up", hl.dsp.global("com.a2tools.dps-meter:toggle"))
hl.bind("CTRL + ALT + L", hl.dsp.global("com.a2tools.dps-meter:lock"))
```

On an older Hyprland that uses `hyprland.conf`:

```
bind = CTRL ALT, R, global, com.a2tools.dps-meter:reset
bind = CTRL ALT, Up, global, com.a2tools.dps-meter:toggle
bind = CTRL ALT, L, global, com.a2tools.dps-meter:lock
```

A key the meter finds already bound is left to your binding, so give the meter's shortcuts keys of their own.

## GNOME: keep the meter above other windows

In a GNOME Wayland session, the meter automatically prefers XWayland when an X11 display is available. This lets its always-on-top request work without installing a GNOME Shell component. Only the meter uses XWayland; the desktop session stays on Wayland. KDE and other desktops keep their default backend.

An explicit `GDK_BACKEND` takes precedence. To select XWayland manually:

```sh
GDK_BACKEND=x11 a2tools-dps-meter
```

Depending on GNOME's fractional-scaling configuration, XWayland text may look softer at 125% or 150%. If text looks blurry, try native Wayland:

```sh
GDK_BACKEND=wayland a2tools-dps-meter
```

If XWayland is unavailable, the meter also falls back to native Wayland. GTK's native Wayland keep-above request has no effect in GNOME: focus the meter, press **Alt+Space**, and select **Always on Top**. Repeat for Details or other meter windows as needed. Close the focused window with **Alt+F4**; on the meter itself this exits the meter, as **Settings > Quit** does.

### Resizing on GNOME

Drag the meter's bottom-right resize handle, or the edges of a tool window. On GNOME with X11, XWayland or native Wayland, the compositor resizes the actual window instead of temporarily expanding a transparent viewport. This avoids the expansion moving the meter back onto the screen. The application detects its actual display backend, including XWayland inside a Wayland session.

KDE Plasma, Hyprland, Sway, i3 and other desktops retain the existing overlay viewport resizing and tool-window edge resizing from 2.0.45. The GNOME path is not enabled on those desktops.

The meter's minimum height is measured from its current content, so shrinking removes empty space without letting the frame overlap the header, rows or footer.

Pausing with the mouse button held does not end the resize. If you release outside the window, move the pointer back over it to resume automatic sizing to the meter's content.

## Build from source (other distributions)

### 1. Install the build tools

You need Rust, Node.js and the system libraries the app builds against, including libpcap for packet capture.

**Rust** (any distribution), then open a new terminal:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Node.js 18 or newer:** from your package manager, or from [nodejs.org](https://nodejs.org).

**System libraries**, for your distribution.

Debian, Ubuntu, Mint, Pop!_OS:

```bash
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev libpcap-dev git
```

Fedora:

```bash
sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file libappindicator-gtk3-devel librsvg2-devel libpcap libcap git
sudo dnf group install c-development
```

Arch, Manjaro, EndeavourOS (if you would rather build than use the package):

```bash
sudo pacman -S --needed webkit2gtk-4.1 base-devel curl wget file openssl appmenu-gtk-module libappindicator-gtk3 librsvg libpcap git
```

**Steam Deck and Steam Machine:** no need to build: follow the [SteamOS instructions](#steam-deck-and-steam-machine-steamos).

### 2. Download and build the meter

The first build takes 10–15 minutes; later ones are much faster. Run these from your home folder or wherever you keep projects:

```bash
git clone https://github.com/taengu/A2Tools-DPS-Meter.git
cd A2Tools-DPS-Meter

# A fresh download lacks some data files the app reads; copy them into place
mkdir -p public/i18n public/data public/src/data
cp -r src/data/i18n/* public/i18n/
cp src/data/skill_icons.json src/data/dot_skill_ids.json public/data/
cp src/data/skill_icons.json src/data/dot_skill_ids.json public/src/data/

npm install
npx tauri build --no-bundle
```

When it finishes, the meter is this one file:

```bash
src-tauri/target/release/a2tools-dps-meter
```

To pick up fixes later: `git pull`, then run the `npx tauri build --no-bundle` line again, then redo step 3.

### 3. Allow the meter to capture packets

Reading the game's network traffic needs a capability Linux only gives on request. Grant it to the meter's file, from the `A2Tools-DPS-Meter` folder:

```bash
sudo setcap cap_net_raw=ep src-tauri/target/release/a2tools-dps-meter
```

- Redo this after every rebuild: a new build is a new file and loses the permission.
- Older versions of this guide also granted `cap_net_admin`. The line above replaces the whole list, so running it again removes that.
- Do not run the meter itself with `sudo`. It would run as root, keep its settings in root's home folder, and often fail to open its window.
- `setcap` does not work on some drives (for example NTFS or exFAT shared with Windows). If it fails, build the meter on a Linux-formatted drive.

Then start it from a terminal, so its output is saved too:

```bash
src-tauri/target/release/a2tools-dps-meter 2>&1 | tee ~/meter-console.log
```

## Sending us your logs

If something does not work, a short logged session tells us why:

1. Start AION 2 from Steam as you normally do.
2. Start the meter from a terminal, with the command for your install above.
3. In the meter, open **Settings**. Under **Diagnostics**, tick **Enable debug logging** and **Enable packet logging**.
4. Log in and enter the world with a character, then wait a minute.
5. Fight a training dummy or some monsters for one to two minutes.
6. While you play, note:
    - whether the meter shows your damage;
    - the ping in the meter's footer, next to the ping the game itself shows;
    - whether the meter stays visible on top of the game, and whether the game is fullscreen, borderless or windowed.
7. Optional, but very useful: change zone or go back to character select once, then fight again briefly.
8. Untick **Enable packet logging**, then close the meter.

Zip and send these files, even if nothing worked: a log of a failure is as useful as a log of success.

| File | Where |
| --- | --- |
| `debug.log` | `~/.local/share/com.a2tools.dps-meter/` |
| The newest `packets_YYYYMMDD_HHMMSS.txt` | `~/.local/share/com.a2tools.dps-meter/` |
| `meter-console.log` | your home folder |

Along with them, tell us:

- your distribution and version (for example Ubuntu 24.04, Fedora 41, CachyOS);
- whether you are on X11 or Wayland: the output of `echo $XDG_SESSION_TYPE`;
- your desktop (KDE Plasma, GNOME or other);
- your Proton version, and whether the game was fullscreen, borderless or windowed;
- whether damage showed up, and roughly when;
- the meter's ping next to the game's own ping;
- whether the meter stayed on top of the game;
- anything else that looked wrong.

The logs contain character names, yours and those of players near you, so send them only to us: on [Discord](https://discord.gg/Aion2Global) or in a [GitHub issue](https://github.com/taengu/A2Tools-DPS-Meter/issues).

## Tiling desktops (Hyprland, Sway, i3)

A tiling window manager tiles every new window unless told otherwise, which breaks an overlay: the meter is squeezed beside the game, and the tooltips on its damage bars appear in the wrong place. Give the meter's window (class `a2tools-dps-meter`) a rule that makes it float, keeps it on every workspace, and turns off blur and borders.

On Hyprland, a player reported this rule works:

```lua
hl.window_rule({
    name = "aion-dps-meter",
    match = {
        class = "^(a2tools\\-dps\\-meter)$",
    },
    float = true,
    pin = true,
    no_blur = true,
    border_size = 0,
})
```

That is Hyprland's Lua configuration. If you use `hyprland.conf` instead, set the same four things (float, pin, no blur, no border) for that window class in its window-rule syntax. On Sway and i3, the equivalent is `floating enable` and `sticky enable` for `app_id`/`class` `a2tools-dps-meter`.

## Troubleshooting

| What you see | What to do |
| --- | --- |
| No update question, though a new version is out | Updates come only to an installed package. A build from source updates with `git pull` and a rebuild. |
| The update asked for no password, or the meter did not come back | Install the current package [by hand](#update-by-hand). |
| Sign-in says the token could not be stored securely | The meter keeps your sign-in in the desktop keyring and never in a plain file. The message says what went wrong. **No desktop keyring is running**: install and start GNOME Keyring (`gnome-keyring`) or KWallet, or turn on Secret Service in KeePassXC; on Hyprland, Sway or i3 start the keyring with your session (for example `exec-once = gnome-keyring-daemon --start --components=secrets`). **Stayed locked**: accept the keyring's unlock prompt. **No collection**: accept the prompt to create a keyring (from 2.0.49 the meter asks for one). Then sign in again. |
| Build fails mentioning `webkit2gtk-4.1`, `pkg-config` or a missing library | Re-run the install line for your distribution. Distributions older than Ubuntu 22.04 lack `webkit2gtk-4.1` and cannot build it. |
| `debug.log` says it failed to load libpcap | Install libpcap (`libpcap0.8` on Debian and Ubuntu, `libpcap` elsewhere), then start the meter again. |
| The meter warns it is not running as admin, or `debug.log` has no `Capture active` lines | The capture permission is missing. Package: reinstall it. Build from source: run the `setcap` line again (a rebuild loses it). |
| `debug.log` says `No AION2 window found` while the game is running | The meter did not find the game process. Send us the output of `ps aux \| grep -i aion` along with your logs. |
| `debug.log` says `Not locked yet` with `0 with game markers` while you fight | Capture sees traffic but not the game's. Tell us if you use a VPN or ping reducer. |
| The window never opens, and the terminal says `Error 71 (Protocol error) dispatching to Wayland display`; or the window is blank or white | WebKit handed its frames to the compositor as GPU buffers, which some setups reject (NVIDIA drivers especially). The meter now has WebKit hand them over in shared memory instead (`WEBKIT_DMABUF_RENDERER_FORCE_SHM=1`). Remove `WEBKIT_DISABLE_DMABUF_RENDERER=1` if you added it to a launcher: on WebKitGTK 2.54 it leaves the window mostly blank. If the window is still wrong, try `GDK_BACKEND=x11 a2tools-dps-meter`, which runs it through XWayland, and tell us. |
| The meter goes behind the game | Run the game borderless or windowed. On GNOME, see [automatic pinning through XWayland and the native Wayland workaround](#gnome-keep-the-meter-above-other-windows). On Wayland an app cannot force itself on top of a fullscreen game. On KDE Plasma, KWin can count a borderless game as fullscreen anyway. From 2.0.54 the meter adds a window rule for itself the first time it starts on KDE (**Layer** forced to **Overlay**, listed in System Settings → Window Management → Window Rules as "A2Tools DPS Meter above fullscreen games"); restart the meter once if it is still behind. On older versions, or if you removed that rule, add it yourself: a rule for window class `a2tools-dps-meter` with **Layer** set to **Overlay**, forced. Thanks to Seralth for this. |
| After a WebKitGTK update the meter's window draws only in pieces, or only while you hover or drag it | WebKitGTK 2.54 no longer draws the transparent overlay fully without its DMA-BUF renderer, which older versions of the meter turned off. Update the meter, or start an older version with `WEBKIT_DISABLE_DMABUF_RENDERER=0 WEBKIT_DMABUF_RENDERER_FORCE_SHM=1 a2tools-dps-meter`. |
| On a tiling desktop the meter is tiled beside the game, or its tooltips appear in the wrong place | See [Tiling desktops](#tiling-desktops-hyprland-sway-i3). |

Stuck on something not listed? Send what you have so far, logs included.
