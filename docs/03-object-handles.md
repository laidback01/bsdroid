# The object handle list

`GetObjectHandles` gives the host a list of objects. An object is a file or a
folder. The operation takes three parameters:

1. The storage identifier.
2. An object format code. A value of 0 means every format.
3. An association handle. An association is a folder.

The third parameter holds two special values, and the standards do not agree
about which value does what.

## The measurement

A Samsung SM-S901U in file transfer mode, with 2059 objects on the storage:

| Third parameter | Objects the device returned |
| --------------- | --------------------------- |
| 0x00000000      | 2059                        |
| 0xffffffff      | 13                          |

The host then read the name of each object in both lists. Both lists start with
the same folders:

```
Pictures  Audiobooks  Alarms  Recordings  Android  Music
Documents  Podcasts  Movies  DCIM  Notifications  Download
```

Every object in the short list reports a parent of 0x00000000, which is the
root.

The reading:

- 0x00000000 gives every object on the storage.
- 0xffffffff gives the objects of the root folder.

## A mistake this measurement corrected

The project first used the opposite names. The code called 0xffffffff
`HANDLE_ALL` and 0x00000000 `HANDLE_ROOT`.

The names came from a reading of the standard, and not from a device. The
device gives the opposite result.

The code now holds the names in `ptp_proto::association`, and each name records
the measurement. Test a new device before you trust a name.

## The host joins many transfers

A data phase is often larger than one USB transfer. The host reads the 12 byte
header first, and the length field says how many bytes follow. The host then
reads again until the count is complete.

`Container::parse` needs the whole container, so `Container::parse` cannot read
the first transfer of a large data phase. `Header::parse` reads the header
alone.

### The test

The handle list of the test device holds 8240 bytes, and 8240 bytes fit in one
read of 64 KiB. The join path therefore did no work, and no test covered the
path.

The environment variable `BSDROID_READ_BUFFER` sets the size of one read. The
variable is a test hook, and not a setting for a user.

| Read size | Reads | Data bytes | Objects |
| --------- | ----- | ---------- | ------- |
| 64 KiB    | 1     | 8240       | 2059    |
| 512 bytes | 17    | 8240       | 2059    |

The two runs give the same bytes and the same object count. The join path works.

`Outcome::reads` holds the count, and `mtpprobe` prints the count. A reader
then sees the evidence, and does not need to trust a calculation.

## An open question

The project has no measurement of image transfer mode. `docs/02-device-states.md`
records that image mode gives the same storage, the same capacity and the same
free space as file transfer mode.

The object count is the test that separates the two modes. A run of
`mtpprobe objects` in image mode answers the question.
