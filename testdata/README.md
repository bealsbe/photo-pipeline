# Test data

`headers/` holds the first few kilobytes of real camera files: all of the
header the capture-time and maker-note parsers in `src/exif.rs` touch, and
none of the picture.  The tests in that file include them with
`include_bytes!`, so `cargo test` needs no photos.

| File | Camera | What it is | Tests (`src/exif.rs`) |
|---|---|---|---|
| `a7v-burst-frame2.jpg.head` | Sony A7 V (ILCE-7M5) | First 4 KB of a JPEG, the second frame of a burst: SOI and the start of the EXIF segment (IFD0, EXIF IFD, the "SONY DSC" maker note's entries). | `sony_jpeg_gives_subseconds_sequence_and_af_point`, `sony_arw_maker_note_has_no_header`, `truncated_or_damaged_headers_lose_metadata_not_the_time` |
| `a7v-portrait.jpg.head` | Sony A7 V (ILCE-7M5) | First 4 KB of a portrait-orientation JPEG (Orientation 8), as above. | `the_af_point_turns_with_the_photo` |
| `a7v-burst-frame1.arw.head` | Sony A7 V (ILCE-7M5) | First 8 KB of an ARW, the first frame of the same burst: IFD0 with its 4 KB XMP packet, the EXIF IFD and the maker note (no header in an ARW). | `sony_arw_maker_note_has_no_header`, `truncated_or_damaged_headers_lose_metadata_not_the_time` |
| `a7rm2-portrait.arw.head` | Sony A7R II (ILCE-7RM2) | First 4 KB of an ARW: whole-second times, a sequence number, a centred AF point. | `older_bodies_give_what_they_have` |
| `t6.cr2.head` | Canon EOS Rebel T6 | First 4 KB of a CR2: IFD0, the EXIF IFD (sub-seconds) and the start of Canon's maker note. | `older_bodies_give_what_they_have`, `truncated_or_damaged_headers_lose_metadata_not_the_time` |

## What was scrubbed

Two fields that identify a device or a file were blanked in place, keeping
every length and offset, so the files parse exactly as before:

- `t6.cr2.head`: the Canon maker note's InternalSerialNumber (tag `0x0096`,
  16 bytes) is all NUL bytes.
- `a7v-burst-frame1.arw.head`: the XMP `xmpMM:DocumentID` and
  `xmpMM:InstanceID` are the nil UUID (`00000000-0000-0000-0000-000000000000`).

The parsers read neither: `src/exif.rs` reads IFD0's make, model,
orientation and date, the EXIF IFD's capture times and sub-seconds, the MPF
index, and from a Sony maker note only FocusLocation (`0x2027`) and the
sequence number (`0xB04A`).  It reads no Canon maker note and no XMP.

## Checked absent

None of the files carries any of these:

- owner, artist or copyright names: IFD0 Artist and Copyright and EXIF
  CameraOwnerName are empty in `t6.cr2.head`, Canon's OwnerName (`0x0009`)
  is empty, and the Sony files have none;
- body or lens serial numbers: EXIF BodySerialNumber (`0xA431`) and
  LensSerialNumber (`0xA435`) in `t6.cr2.head`, Canon's LensInfo (`0x4019`)
  and Sony's enciphered `0x9050` block (which holds the Sony serial) all
  point past the end of the excerpt, and no Sony file has a SerialNumber
  tag (`0x2031`);
- GPS: only `t6.cr2.head` has a GPS IFD pointer, and the IFD lies past the
  end of the excerpt;
- XMP or EXIF IDs other than the blanked ones above (no ImageUniqueID).

Checked by walking every IFD and maker-note entry in each file, and by
searching each excerpt for the original files' serial numbers (none found).

Kept on purpose: the capture times, sub-seconds, time-zone offsets, camera
models, firmware versions and the Canon lens model.  The tests assert the
times, and rewriting EXIF or maker-note bytes would risk the burst and
capture-time tests for little privacy gain.

## Adding a file

Take only the header (`head -c 4096 photo.ARW > headers/<name>.head`, or
more when it runs further, as the A7 V ARW's XMP packet makes it).  Before
committing, blank any serial number, owner name or GPS data in place,
keeping every length, and zero any XMP or EXIF IDs.  Add a row above.
