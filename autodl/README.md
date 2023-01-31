# autodl

## Config file format

In `autodl.toml` in the root dir:

```toml
[global]
port = 8181
# ... any other rocket-rs options
log_dir = "./logs"
ytdlp_path = "./yt-dlp"
delete_files_after_move = true

[[global.output_directories]]
source="default" # output directory for yt-dlp
destination_remote= {destination="padenot@example.net:/some/writable/path", extra_args="--rsync-path=/usr/bin/rsync -e \"/usr/bin/ssh -p 2211  -o Compression=no -T -x\""}

[[global.output_directories]]
source="default" # output directory for yt-dlp
destination_local = "/some/other/path/"
```

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
