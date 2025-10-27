#!/bin/sh

linuxkit build --name bootstrap --format kernel+initrd ./bootstrap-layout.yml
linuxkit build --name customer --format kernel+initrd /workspace/layout.yml

eif_build \
  --kernel /blobs/bzImage \
  --kernel_config /blobs/bzImage.config \
  --cmdline "$(cat /blobs/cmdline)" \
  --ramdisk bootstrap-initrd.img \
  --ramdisk customer-initrd.img \
  --output /out/$1.eif

cd /out
sha256sum $1.eif > $1.eif.sha256sum

echo "SHA256 checksum:"
cat $1.eif.sha256sum
