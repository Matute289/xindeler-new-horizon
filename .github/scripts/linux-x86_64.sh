#!/bin/bash
export VELOREN_USERDATA_STRATEGY=executable;
time cargo build --release --no-default-features --features default-publish;

objcopy --compress-debug-sections=zlib target/release/xindeler-server-cli target/release/xindeler-server-cli-compressed
objcopy --compress-debug-sections=zlib target/release/xindeler-voxygen target/release/xindeler-voxygen-compressed
mv target/release/xindeler-server-cli-compressed target/release/xindeler-server-cli
mv target/release/xindeler-voxygen-compressed target/release/xindeler-voxygen
