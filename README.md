# auto-dl

Periodically download a youtube playlist, extract audio, convert to mp3, move
to directory (possibly synced using syncthing).

- drop https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp in this directory
- add playlist informations to ./playlists.txt. format: 3 elements per line (no
  spaces, space is for separators):

```
name yt-playlist-url absolute-path-to-destination
name2 yt-playlist-url2 absolute-path-to-destination2
...
```

- add a cron/scheduled task to run it, or run it manually
- make the destination dir to be a syncthing directory shared with other devices
- fix perms so that this script can write in the syncthing directory
- 
