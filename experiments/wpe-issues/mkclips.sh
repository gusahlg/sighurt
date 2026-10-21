#!/usr/bin/env bash
# Makes the test clips (testsrc2, 3 s) in pages/: needs ffmpeg (the repo's nix-shell has it).
set -eu
cd "$(dirname "$0")/pages"
src="testsrc2=size=320x180:rate=25:duration=40"
ffmpeg -loglevel error -y -f lavfi -i "$src" -c:v libx264 -pix_fmt yuv420p -movflags +faststart h264.mp4 || echo "no libx264"
ffmpeg -loglevel error -y -f lavfi -i "$src" -c:v libvpx-vp9 -pix_fmt yuv420p vp9.webm || echo "no vp9"
ffmpeg -loglevel error -y -f lavfi -i "$src" -c:v libsvtav1 -pix_fmt yuv420p av1.mp4 || ffmpeg -loglevel error -y -f lavfi -i "$src" -c:v libaom-av1 -cpu-used 8 -pix_fmt yuv420p av1.mp4 || echo "no av1"
ls -la
