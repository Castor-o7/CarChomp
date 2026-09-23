#!/bin/bash -e
# Install the release bundle in files/carchomp into the image, as
# `deploy/install.sh --kiosk <first user>` would on a running Pi. It runs in a
# chroot without systemd: the installer's have_systemd check skips starting
# and enabling services, so this enables them afterwards and stops the
# PostgreSQL server the installer started to create the database.
src=/var/tmp/carchomp-bundle
rm -rf "${ROOTFS_DIR}${src}"
cp -a files/carchomp "${ROOTFS_DIR}${src}"

on_chroot <<CHROOT
set -e
${src}/deploy/install.sh --kiosk "${FIRST_USER_NAME}" ${CARCHOMP_DEMO:+--demo}
service postgresql stop
systemctl enable postgresql carchompd
if [ -f /etc/systemd/system/carchomp-sim.service ]; then systemctl enable carchomp-sim; fi
if [ -d /etc/lightdm ]; then
	# The kiosk starts with the desktop session: log straight in.
	mkdir -p /etc/lightdm/lightdm.conf.d
	printf '[Seat:*]\nautologin-user=%s\n' "${FIRST_USER_NAME}" >/etc/lightdm/lightdm.conf.d/50-carchomp.conf
fi
rm -rf ${src}
CHROOT
