---
name: sonos
description: "Control Sonos speakers: playback, volume, grouping."
version: 0.1.0
author: Hermes Agent
license: MIT
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [Sonos, Smart-Home, Audio]
    related_skills: [openhue]
    homepage: https://github.com/avantrec/soco-cli
prerequisites:
  commands: [sonos]
---

# Sonos (SoCo-CLI)

Control Sonos speakers from the terminal by wrapping [avantrec/soco-cli](https://github.com/avantrec/soco-cli) (Apache-2.0), the standard Python CLI for Sonos built on the SoCo library. It works entirely over the local network (UPnP) — no Sonos cloud account needed. This skill's text is MIT; the upstream tool remains Apache-2.0.

## When to Use

- "Play/pause/stop music on the Kitchen speaker"
- "Turn the volume down in the Living Room"
- "Group the Study with the Kitchen" / "party mode"
- "Play my Jazz favourite" or a radio station
- Queue management, track info, sleep timers, playing local audio files

## Prerequisites

```bash
# Recommended: self-contained install (upstream recommends pip or pipx)
pipx install soco-cli        # or: uv tool install soco-cli
# Or plain pip:              pip install -U soco-cli
```

- The machine running Hermes must be on the **same LAN/subnet** as the speakers (SSDP multicast discovery).
- Installs `sonos`, `sonos-discover`, and `sonos-http-api-server` onto PATH.

## How to Run

Run all commands through the `terminal` tool. Command shape is always:

```bash
sonos SPEAKER ACTION [parameters]
```

`SPEAKER` is the Sonos room name (partial, case-insensitive matches work — `kit` matches `Kitchen`) or an IPv4 address. Chain commands with ` : ` (spaces required): `sonos Kitchen volume 25 : Kitchen play`. Use `_all_` as the speaker name to target every speaker. Exit code 0 = success; setters print nothing.

## Quick Reference

| Command | Purpose |
|---|---|
| `sonos-discover` | Discover speakers; build/refresh the local speaker cache |
| `sonos <spkr> play` / `pause` / `stop` | Playback control |
| `sonos <spkr> next` / `previous` | Track skip |
| `sonos <spkr> volume` | Get volume (0–100) |
| `sonos <spkr> volume 25` | Set volume |
| `sonos <spkr> relative_volume -10` | Adjust volume by delta |
| `sonos <spkr> mute on\|off` | Mute control |
| `sonos <spkr> track` | Info on the currently playing track |
| `sonos <spkr> state` | Playback state (PLAYING, STOPPED, …) |
| `sonos <spkr> group <coordinator>` | Group speaker with `<coordinator>` |
| `sonos <spkr> ungroup` | Remove speaker from its group |
| `sonos <spkr> party_mode` | Group all speakers together |
| `sonos <spkr> groups` | List all groups in the system |
| `sonos <spkr> play_favourite "<name>"` | Play a Sonos favourite (alias `play_fav`) |
| `sonos <spkr> list_favs` | List Sonos favourites |
| `sonos <spkr> line_in [<other_spkr>] on` | Switch to a Line-In input and play |
| `sonos <spkr> list_queue` | Show the queue |
| `sonos <spkr> play_from_queue <n>` | Play queue item n (from 1) |
| `sonos <spkr> add_playlist_to_queue "<name>"` | Queue a Sonos playlist |
| `sonos <spkr> clear_queue` | Clear the queue |
| `sonos <spkr> play_uri <url>` | Play a stream URL |
| `sonos <spkr> play_file <path>` | Stream a local audio file to the speaker |
| `sonos <spkr> sleep_timer 30m` | Set a sleep timer |
| `sonos <spkr> zones` | List all room names |
| `sonos <spkr> sysinfo` | Table of all speakers in the system |
| `sonos --actions` | Full list of available actions |

## Procedure

1. **Discover**: run `sonos-discover` once to find speakers and cache them (`~/.soco-cli`). Use `sonos-discover -p` to print the cached list. If discovery is slow or flaky, prefer the cache: `sonos -l <spkr> <action>`.
2. **Target the speaker** by room name (quote names with spaces: `sonos "Living Room" play`) or IP address.
3. **Run the action.** Check exit code; errors go to stderr.
4. **Grouping workflow**: `sonos Study group Kitchen` groups Study *with* Kitchen (Kitchen is coordinator). Set group level with `group_volume`, then later `sonos Study ungroup` (or `ungroup_all` from any speaker to dissolve everything).
5. **Volume etiquette**: read `sonos <spkr> volume` before large changes; prefer `relative_volume` or `ramp_to_volume` over jumping straight to high absolute values.
6. **Interactive shell** (`sonos -i [speaker]`) exists for humans; from Hermes prefer single commands or ` : ` chains — they're script-friendly and return exit codes.
7. **HTTP API server** (long-running): start with `terminal` `background=true`: `sonos-http-api-server` (default port 8000, `-p` to change). Then `curl http://localhost:8000/<speaker>/<action>/<params>` returns JSON with an `exit_code` field. `/speakers` lists speakers; `/docs` has OpenAPI docs.

## Pitfalls

- **Discovery requires same subnet + multicast** (SSDP, UDP 1900). Firewalls blocking incoming UDP responses or TCP 1400-1499 break discovery/wait actions; soco-cli falls back to a slower network scan.
- **Speaker names with spaces need quotes** — double quotes work on all platforms.
- **Slow discovery**: build the local cache with `sonos-discover`, then always pass `-l` (`sonos -l Kitchen play`) or set `USE_LOCAL_CACHE=TRUE`. Refresh after renames/IP changes with `sonos-discover` again.
- **Partial name matches must be unambiguous**, or sonos returns an error.
- **No confirmation prompts**: queue/playlist mutations execute immediately.
- **`_all_` works with every action** — no safety checks, use with caution.
- Actions on a grouped (non-coordinator) speaker may be redirected to the coordinator (e.g. queue ops); volume stays per-speaker.
- There is **no built-in TTS/`speak` action**; play pre-rendered audio with `play_file` instead.

## Verification

```bash
sonos --version        # prints SoCo-CLI, SoCo, and Python versions
sonos-discover         # should list your speakers with names and IPs
sonos <speaker> state  # confirms end-to-end control of one speaker
```
