# A Raspberry Pi OS image with CarChomp

`stage-carchomp` is a [pi-gen](https://github.com/RPi-Distro/pi-gen) stage
that runs `deploy/install.sh --kiosk <first user>` from a release bundle
inside the image, on top of the Raspberry Pi OS desktop. Flash the result and
the Pi boots straight into the map. The release workflow
(`.github/workflows/release.yml`) builds it for every `v*` tag; this is how to
do it by hand.

## Build

On an arm64 Linux machine with Docker (a Pi 5, or any arm64 host; an x86 host
works too, with qemu-user-static, but emulates every step and is slow):

    tools/bundle.sh                     # in this repository -> target/bundle/carchomp-<version>-arm64.tar.gz
    git clone --branch arm64 https://github.com/RPi-Distro/pi-gen   # 64-bit Bookworm
    cp -r deploy/pi-gen/stage-carchomp pi-gen/
    mkdir pi-gen/stage-carchomp/00-install/files
    tar -xzf target/bundle/carchomp-*-arm64.tar.gz -C pi-gen/stage-carchomp/00-install/files
    touch pi-gen/stage2/SKIP_IMAGES pi-gen/stage4/SKIP_IMAGES      # only export ours
    cat >pi-gen/config <<'END'
    IMG_NAME=carchomp
    RELEASE=bookworm
    STAGE_LIST="stage0 stage1 stage2 stage3 stage4 stage-carchomp"
    FIRST_USER_NAME=pi
    FIRST_USER_PASS='choose one'
    DISABLE_FIRST_BOOT_USER_RENAME=1
    DEPLOY_COMPRESSION=xz
    END
    cd pi-gen && ./build-docker.sh

The image lands in `pi-gen/deploy/` as `<date>-carchomp.img.xz`.
The stage has to live inside the pi-gen directory, as above, for
`build-docker.sh` to see it.

- Stages 0 to 4 are the stock Raspberry Pi OS desktop, which brings labwc and
  Chromium for the kiosk.
- `DISABLE_FIRST_BOOT_USER_RENAME=1` keeps the first-boot wizard from
  renaming the user. The kiosk autostart lives in that user's home, and
  `/etc/carchomp/install.env` names them for later updates.
- Add `export CARCHOMP_DEMO=1` to the config to build an image that runs the
  simulator in place of GPS and radio (`--demo`).
- SSH stays off unless the config says `ENABLE_SSH=1`.

## What differs from installing on a running Pi

The installer runs in a chroot, where nothing can be started:

- `have_systemd` is false, so the installer starts nothing. It still runs
  `systemctl enable`/`disable` where that only edits files (the firewall on,
  avahi's boot start off); the stage enables `postgresql`, `carchompd` (and
  `carchomp-sim` for a demo image) itself. They start on first boot.
- `nft` cannot check the firewall rules against the build machine's kernel
  (under qemu it cannot talk to one at all), so the installer warns and
  installs them unchecked; `carchomp-firewall.service` loads them at first
  boot.
- PostgreSQL is started with `service postgresql start` to create the
  database and the PostGIS extension, then stopped again by the stage.
  carchompd creates its tables on first boot.
- The desktop is set to log the first user in automatically (a LightDM
  drop-in), since the kiosk starts with the desktop session.
- The Wi-Fi hotspot cannot be set up: NetworkManager is not running. After
  first boot, set the Wi-Fi country and run `deploy/install.sh --kiosk pi
  --hotspot SSID` from a release bundle; it asks for the passphrase.
- The map fonts and sprites and the `pmtiles` tool are downloaded while the
  image is built, so the image works offline from the start.
