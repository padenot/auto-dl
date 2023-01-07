#!/bin/sh

CURL=`which curl`

[[ -z $CURL ]] && "This needs a curl binary."

$CURL -L -o yt-dlp https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp
chmod +x yt-dlp
