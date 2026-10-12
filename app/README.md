# LapAnalysis app

Download AiM Solo 2 DL sessions over Wi-Fi and browse them locally, without RaceStudio 3.
The logger's protocol is summarised at the top of `src/aim.rs`.

## Run

```sh
cd app
cargo build --release              # once, while online (fetches libxrk from GitHub)
./target/release/lapanalysis       # web app at http://127.0.0.1:8080
```

1. Turn Wi-Fi on on the Solo and join its network from the Mac.
2. Open the **Logger** tab. It finds the Solo, lists its sessions, and downloads the new ones.
3. Back on your normal Wi-Fi, open **Library** to view sessions: laps, a speed-coloured GPS
   map, and synced channel charts (drag to zoom, double-click to reset).

Everything is served from the binary, so it works with no internet on the Solo's Wi-Fi.

Sessions are saved in `library/` at the repo root. The app creates it on first run and lists
whatever is in it, so copying .xrz files in works too. It's git-ignored. Use `--library DIR`
or `LAPANALYSIS_LIBRARY` to keep them somewhere else.

## CLI

```sh
lapanalysis list          # sessions on the logger
lapanalysis pull          # download every session not in the library (or: pull a_0126.xrz)
lapanalysis info FILE     # laps and channels of a downloaded file
lapanalysis serve --port 8080
```

## Develop without the Solo

`cargo test` runs the client against an in-process fake logger. For UI work, serve a
folder of .xrz files as a fake logger and point the app at it:

```sh
lapanalysis fake-solo ../library            # optional: --csv <summary.csv>
lapanalysis --solo-ip 127.0.0.1 --solo-tcp-port 2001 --solo-udp-port 36012 --udp-local-port 0 \
            --library /tmp/lap-lib serve
```
