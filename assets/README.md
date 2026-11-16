# Application icon

`icon.png` is an exact, unmodified copy of Terraria's Steam library icon for
Steam app 105600 (128x128 RGBA). The tree sprite is artwork by Re-Logic.

It is included only so this personal tool has a recognisable icon in the title
bar, taskbar, application menu and file managers. It is **not** covered by the
MIT license that applies to the source code, and it should not be redistributed
separately from the tool.

Related files, all generated from `icon.png`:

- `icon.ico` - 16/24/32/48/64/128 pixel entries for the Windows executable
  resource, embedded by `build.rs` when building on Windows.
- The macOS `.icns` is generated at bundle time by `scripts/package-macos.sh`
  (nothing is committed for it).

At startup the icon is passed to the windowing system as-is. If a larger icon is
ever dropped in, the app downscales it to 192x192 because a 256x256 RGBA image
is slightly larger than the maximum X11 request size, which would make the
window icon silently fail on X11.
