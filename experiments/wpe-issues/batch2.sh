#!/usr/bin/env bash
here=$(cd "$(dirname "$0")" && pwd)
python3 -m http.server 18765 --bind 127.0.0.1 --directory $here/pages > $here/out/http.log 2>&1 &
srv=$!
sleep 1
u=http://127.0.0.1:18765/video.html
D="GST_DEBUG=2,webkitmediaplayer:5,webkitvideosink:6,webkit*:4,glimagesink:4,decodebin*:3,GST_ELEMENT_FACTORY:3,GST_AUTOPLUG:4"
export WAIT=10 GST_PLUGIN_FEATURE_RANK=fakeaudiosink:MAX
$here/run.sh h_base $u
$here/run.sh h_dbg $u "$D"
$here/run.sh h_nodmabuf $u WEBKIT_GST_DMABUF_SINK_DISABLED=1
$here/run.sh h_nogl $u WEBKIT_GST_DISABLE_GL_SINK=1
$here/run.sh h_nogl_nodmabuf $u WEBKIT_GST_DISABLE_GL_SINK=1 WEBKIT_GST_DMABUF_SINK_DISABLED=1
kill $srv
