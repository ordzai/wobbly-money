# wobbly-money

Command-line tool that backs up Wobbly Life save data and sets the Wobbly
Island money and Space money of every character in every save slot. It builds
to a single executable for Linux and Windows and needs no runtime.

## Usage

Close Wobbly Life before running.

```
wobbly-money [OPTIONS]
```

| Option | Description |
|---|---|
| `--money <N>` | Wobbly Island money. Default `999999999`, maximum `2147483647`. |
| `--space-money <N>` | Space money. Default: the `--money` value. |
| `--save-dir <PATH>` | Wobbly Life `Save` directory. Default: detected automatically. |
| `--backup-dir <PATH>` | Root directory for backups and logs. Default: `<Save>/../SaveBackups`. |
| `--log <PATH>` | Log file. Default: `<backup root>/<timestamp>.log`. Appends if the file exists. |
| `--dry-run` | Report current values. Modifies no save files and creates no backup. |
| `--backup-only` | Create a backup. Modifies no save files. |
| `-h`, `--help` | Print help. |

Numbers may contain `,` or `_` separators (`--money 1,000,000`).

Examples:

```
wobbly-money --dry-run
wobbly-money
wobbly-money --money 50000 --space-money 20000
```

On Windows, starting the executable from Explorer (no arguments) runs with
the defaults and waits for Enter before closing the console window.

The default of 999,999,999 is below the Int32 maximum so that money earned in
the game does not overflow the stored value.

## Save directory detection

- **Windows:** `%USERPROFILE%\AppData\LocalLow\RubberBandGames\Wobbly Life\Save`
- **Linux (Proton):** for each Steam installation (`~/.local/share/Steam`,
  `~/.steam/steam`, `~/.steam/root`, Flatpak, and Snap locations), every
  library listed in `steamapps/libraryfolders.vdf` is checked for
  `steamapps/compatdata/1211020/pfx/drive_c/users/steamuser/AppData/LocalLow/RubberBandGames/Wobbly Life/Save`.

Each subdirectory of `Save` named by a 64-bit Steam ID is processed as an
account. Use `--save-dir` if detection fails.

## Behavior

For each account:

1. **Backup** (all modes except `--dry-run`): the entire account directory
   (`GameSaves`, all `SaveSlot_*` directories, and `Settings`) is copied to
   `<backup root>/<SteamID>/<UTC timestamp>/`. Restore by copying it back
   over the account directory while the game is closed.
2. **Parse**: every `GameSaves/SaveSlot_*/PlayerData_*.sav` file (one per
   character) is read and parsed completely.
3. **Modify** (default mode only): files whose values differ from the target
   are rewritten. Files already at the target are left unchanged.
4. **Verify**: the modified data is parsed again in memory and the money
   values are compared with the target. On a mismatch the program exits with
   an error and the original file is left unchanged.
5. **Replace**: the data is written to `<file>.tmp` and renamed over the
   original.

Only the money values are changed; all other bytes are copied unchanged.

### Log file

Every run writes a log. For each `PlayerData` file it contains:

- the parsed record listing of the original file, with the money values
  marked `<-- Wobbly Island money` and `<-- Space money`;
- each change, with its byte offset, old and new value, and the string length
  change where applicable;
- the parsed record listing of the modified data, written before the rename.

### Steam Cloud

Steam uploads the modified files at the next game launch. If Steam reports a
cloud conflict, select the local files.

## Save format

`.sav` files are .NET `BinaryFormatter` streams, specified in
[MS-NRBF](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-nrbf/).
The files contain no checksum.

| Value | Location in `PlayerData_N.sav` | Encoding |
|---|---|---|
| Wobbly Island money | `PlayerMiscData.money` | Int32 member |
| Space money | `PlayerActiveExternalData.data` | `"money"` key in a JSON string |

A character has Space data only after visiting Space. Characters without it
receive Wobbly Island money only; the tool does not create Space data.
NRBF strings are length-prefixed and the format contains no absolute offsets,
so a JSON string can change length without affecting the rest of the file.

`PlayerSettingsData_N.sav` contains per-character control settings and is not
modified.

## Building

Requires Rust 1.88 or later (edition 2024).

```
cargo build --release
```

Windows executable, cross-compiled from Linux (requires `mingw-w64-gcc`):

```
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

Outputs:

- `target/release/wobbly-money`
- `target/x86_64-pc-windows-gnu/release/wobbly-money.exe`
