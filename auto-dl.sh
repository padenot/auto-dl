#!/bin/sh -xe

# $1 playlist name
# $2 playlist URL
auto_dl_one_playlist() {
	./yt-dlp --download-archive "$1-list.txt" --ignore-errors --format bestaudio --extract-audio --audio-format mp3 --audio-quality 320K  -o "%(playlist)s/%(playlist_index)s - %(title)s.%(ext)s" $2
}

while read -r line; 
do 
	playlist_dir="$(cut -d' ' -f1 <<<$line)"
	yt_url="$(cut -d' ' -f2 <<<$line)"
	output_dir="$(cut -d' ' -f3 <<<$line)"
	auto_dl_one_playlist $playlist_dir $yt_url
	pushd $playlist_dir
	rsync --remove-source-files -r --progress . $output_dir
	popd
done < playlists.txt
