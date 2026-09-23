#!/bin/bash -e
if [ ! -f 00-install/files/carchomp/VERSION ]; then
	echo "stage-carchomp: extract a release bundle into 00-install/files/ first (see README.md)" >&2
	exit 1
fi
if [ ! -d "${ROOTFS_DIR}" ]; then
	copy_previous
fi
