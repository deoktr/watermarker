# watermarker

A small, self-contained Linux CLI that puts **visible** and **invisible**
watermarks on PNG and JPEG images. Built in Rust and linked statically: the
delivered binary needs no libraries, fonts, ImageMagick, or ExifTool at runtime.

It is designed for the "I must send this document/photo online, but I want to be
able to prove where a leak came from" case: a passport scan, an ID photo, a
private screenshot. But also prevent reuse of leaked document, one way is by
explicitly stating who is the intended recipient.

```text
watermarker add [OPTIONS] <FILE|DIR>...
watermarker verify [OPTIONS] <FILE|DIR>...
```

`add` is the default, so `watermarker photo.jpg` works.

## Quick start

```sh
# Add the default tiled diagonal watermark and both invisible layers
./dist/watermarker passport.jpg

# One large watermark in the corner, 40% opacity, custom text and recipient id
./dist/watermarker --single --position bottom-right --opacity 40 \
    -t "CONFIDENTIAL - {id}" --id case42-alice passport.jpg

# Batch, separate output folder, unique note in the hidden payload
./dist/watermarker --hidden "recipient: bob@example.org" -o watermarked/ photos/*.jpg

# Later: inspect a leaked file
./dist/watermarker verify leaked.jpg
./dist/watermarker verify --json watermarked/
```

Output is written next to each input as `name.wm.png` / `name.wm.jpg`
(configurable with `--suffix` and `-o`). Inputs are never overwritten.

## Visible watermark

| Option          | Default        | Meaning                                                                                                                   |
| --------------- | -------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `-t, --text`    | `CONFIDENTIAL` | Text to draw. Supports `{file}`, `{date}`, `{time}`, `{id}`. `\n` makes multiple lines.                                   |
| `-O, --opacity` | `28`           | 0-100.                                                                                                                    |
| `-a, --angle`   | `45`           | Degrees. 45 = diagonal, 90 = vertical, 0 = horizontal.                                                                    |
| `-s, --size`    | `4.5`          | Text height as % of the shorter image side.                                                                               |
| `--single`      | off            | Draw once instead of tiling.                                                                                              |
| `--position`    | `center`       | `center`, `top-left`, `top`, `top-right`, `left`, `right`, `bottom-left`, `bottom`, `bottom-right`. Used with `--single`. |
| `--gap`         | `200`          | Spacing between tiled copies, % of text line height.                                                                      |
| `--color`       | `#ffffff`      | `#RRGGBB` or `white`, `black`, `red`, `gray`, `yellow`, `blue`.                                                           |
| `--no-outline`  | off            | Disable the dark outline that keeps light text readable on light photos.                                                  |

The font (Roboto Bold, Apache-2.0) is embedded in the binary.

## Invisible watermark

Two independent layers are written, both carrying the same JSON record:

1. **Metadata (XMP).** A minimal XMP packet with a base64 copy of the record is
   stored in the standard XMP location (JPEG APP1 / PNG `iTXt`), plus a
   human-readable JPEG comment.
2. **Steganography in the image itself.**
   - **PNG:** keyed LSB replacement in the R/G/B samples. Lossless file handling
     preserves it bit-exactly.
   - **JPEG:** keyed LSB replacement in the magnitudes of quantised DCT
     coefficients. The entropy stream is decoded and re-encoded with the file's
     own Huffman tables, so there is no extra generation loss.

The record looks like:

```json
{"v":1,"id":"9f2c...","unix":1763644800,"utc":"2026-11-20T...Z",
 "file":"passport.jpg","text":"CONFIDENTIAL","note":"recipient: alice"}
```

Bits are scattered by a Feistel permutation seeded from `--key`, and every bit
is stored three times with majority voting. A CRC32 rejects modified/truncated
payloads. Extraction is possible with the same binary and key:

```text
$ watermarker verify leaked.jpg
leaked.jpg  (jpeg)
  metadata watermark : FOUND (XMP)
  hidden DCT-LSB : FOUND (integrity OK, 165893 carrier slots)
  id       : fda9f30fe95c759c
  created  : 2026-11-20T14:26:46Z
  file     : passport.jpg
  text     : CONFIDENTIAL
  note     : case-42 / jane@example.com
  layers   : consistent (identical record)
```

Use `--json` for machine-readable output.

### Metadata stripping

Every input is fully decoded and re-encoded, so **all** original EXIF, GPS,
camera, software, IPTC, ICC and XMP blocks are discarded. The only metadata in
the output is the watermark packet described above. (JPEG re-encoding also
writes a fresh JFIF header.)

EXIF orientation is applied to the pixels before the tag is stripped, so rotated
phone photos keep their correct orientation.

## Options shared by `add` and `verify`

| Option           | Default                      | Meaning                                                     |
| ---------------- | ---------------------------- | ----------------------------------------------------------- |
| `--key`          | `watermarker-default-key-v1` | Seed for stego bit placement. Use the same value to verify. |
| `--no-recursive` | off                          | Do not descend into directories.                            |

Additional `add` switches: `--no-visible` (invisible only), `--no-invisible`
(visible only; metadata is still stripped), `-q/--quality` (JPEG, default 92),
`--id` (fixed record id).

## Building

Requirements: Rust toolchain, Linux x86_64. The repository already contains the
statically linked `dist/watermarker`.

```sh
./build.sh
```

The script runs `cargo rustc --release -- -C target-feature=+crt-static` and
verifies that the result has no `NEEDED` entries. A static binary has no
interpreter and runs on any x86_64 Linux kernel, regardless of the
distribution's libc.

## How well does this protect a file?

Be clear about the threat model: these are **deterrents and tracing aids**, not
DRM.

- The visible layer stops casual reuse and makes a leaked screenshot obvious.
- The XMP layer identifies the recipient in any tool that preserves metadata.
- The stego layer survives a bit-exact copy, upload to cloud storage, file
  sharing, and screenshots of the *file* (not of the screen), even if metadata
  is stripped.
- **Recompression, resizing, rotating, or cropping destroys the LSB/DCT
  payload.** Most messengers and social networks recompress images, so send the
  original file, ideally in an archive, when you need the hidden layer to
  survive. PNG survives any lossless re-save; JPEG does not survive re-saving.
- Metadata can be removed trivially by anyone. The stego layer is the fallback,
  and both are gone after a print/scan cycle.
- Very small or very flat JPEGs may not contain enough usable DCT coefficients
  for the requested payload. The tool fails that file with the exact slot
  shortfall rather than silently producing an image without the hidden layer.
- The payload is encoded, not encrypted. Anyone with this binary and the key can
  read it. Treat it as a serial number that links a copy to a recipient, and
  keep a private ledger mapping `--id` to the person who received that copy. Do
  not put secrets in `--hidden`.

For stronger robustness (recompression/resize resistance) you would need a
proper spread-spectrum/DCT watermark with error correction, which trades payload
size and image quality. The current design intentionally favours "provably
present, invisible, exact" over "survives anything".

## Layout

```text
src/main.rs        CLI, file discovery, add/verify pipeline
src/visible.rs     text rasterisation, rotation, tiling, alpha blending
src/stego.rs       payload frame, Feistel slot permutation, 3x repetition
src/png_stego.rs   PNG RGB LSB layer
src/jpeg_stego.rs  baseline-JPEG entropy decoder/encoder + DCT LSB layer
src/metadata.rs    XMP construction, PNG iTXt and JPEG APP1/COM injection
src/payload.rs     watermark record
src/datetime.rs    UTC formatting without external date crates
assets/            embedded font + license
```

Third-party code: [Roboto](https://fonts.google.com/specimen/Roboto) Bold,
Apache License 2.0 (`assets/LICENSE-Roboto.txt`).

## License

watermarker is licensed under [MIT](./LICENSE).
