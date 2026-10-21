#!/usr/bin/env bash
here=$(cd "$(dirname "$0")" && pwd)
u=file://$here/pages/video.html
$here/run.sh base $u
$here/run.sh gstdebug $u GST_DEBUG=3,webkit*:5 
