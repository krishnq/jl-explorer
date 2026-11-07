# Third-party notices

JL Explorer's own code is MIT-licensed (see [LICENSE](LICENSE)). It includes, or is derived
from, the following third-party work.

## JieLi SDK: BR25 flash loader (Apache-2.0)

- **File:** `assets/br25loader.bin`, included unmodified.
- **Author:** Zhuhai Jieli Technology Co., Ltd. (JieLi)
- **Source:** <https://github.com/Jieli-Tech/fw-AC63_BT_SDK>, `cpu/br25/tools/br25loader.bin`
  (last changed in commit `106ccc41be2e105435ee8ad4dc6a41a0028550b5`)
- **License:** Apache License 2.0. Full text in [LICENSES/Apache-2.0.txt](LICENSES/Apache-2.0.txt).
- **SHA-256:** `6cb7f0f9cf6e172d5b698eade179ecffcc4406c834c9664de376c430a160ea83`

The app uploads this loader into the speaker's RAM so it can read and write the flash.

## JieLi SDK: format information (Apache-2.0)

The same SDK was used as a reference for interoperability. Configuration item IDs, field
layouts and EQ filter-type numbers in `src/formats/` follow its public headers
(`include_lib/system/syscfg_id.h`, `include_lib/system/user_cfg.h`,
`include_lib/media/media_develop/media/cpu/br25/asm/hw_eq.h`). No SDK source code is copied.

## jl-uboot-tool and jl-misctools (MIT)

Copyright (c) 2023 Andrey Grigoryev (kagaimiq).

- <https://github.com/kagaimiq/jl-uboot-tool>: the USB download-mode protocol in
  `src/device.rs` is ported from it.
- <https://github.com/kagaimiq/jl-misctools>: the firmware ciphers, chip-key decoding and
  flash image layout in `src/jlfw.rs` and `src/formats/isdcfg.rs` are ported from it.

Both are under the MIT License. Full text in [LICENSES/MIT-kagaimiq.txt](LICENSES/MIT-kagaimiq.txt).

## jielie

<https://github.com/kagaimiq/jielie>: background documentation on JieLi chips. No content
is copied from it.

## Trademarks

JieLi and Philips are trademarks of their respective owners. JL Explorer is not affiliated
with or endorsed by them; names are used only to describe compatibility.
