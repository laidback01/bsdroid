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

`Config::read_buffer` sets the size of one read. A test gives the session a
small value, and a device that answers with more bytes than the value then uses
the join path.

| Read size | Reads | Data bytes | Objects |
| --------- | ----- | ---------- | ------- |
| 64 KiB    | 1     | 8240       | 2059    |
| 512 bytes | 17    | 8240       | 2059    |

The two runs give the same bytes and the same object count. The join path works.

`Outcome::reads` holds the count, and `mtpprobe` prints the count. A reader
then sees the evidence, and does not need to trust a calculation.

## Image transfer mode gives a part of the storage

The same cellphone, in image transfer mode:

| Item                       | File transfer | Image transfer |
| -------------------------- | ------------- | -------------- |
| Objects on the storage     | 2059          | 821            |
| Objects in the root folder | 13            | 2              |
| Capacity                   | 239935107072  | 239935107072   |
| Free space                 | the same      | the same       |

The root folder in file transfer mode:

```
Pictures  Audiobooks  Alarms  Recordings  Android  Music
Documents  Podcasts  Movies  DCIM  Notifications  Download  Screenshots
```

The root folder in image transfer mode:

```
Pictures  DCIM
```

Image transfer mode gives the two folders that hold photographs. The mode hides
the other 11 folders, and the mode hides 1238 objects.

### Why the storage test is not enough

The capacity and the free space are the same in both modes. A host that reads
the storage alone finds no difference.

The object count is the only test that shows the difference.

### The count is not a property of the modes

The two counts, 2059 and 821, come from one cellphone with one set of files. The
owner of the test cellphone makes photographs and videos, and keeps few other
files. A cellphone with much music gives a much larger difference.

Do not use the ratio. Use the folder list, which does not change with the
files:

- Image transfer mode gives `Pictures` and `DCIM`.
- File transfer mode gives those two folders, and 11 more.

### What a photograph user gets

`Screenshots` and `Messages` are folders inside `Pictures`. Image transfer mode
gives `Pictures`, so image transfer mode gives both folders.

A user who wants a photograph, a video, a screenshot or an image from a message
loses nothing in image transfer mode.

`Download` is a folder in the root, and image transfer mode hides the root
folders other than `Pictures` and `DCIM`. A file in `Download` is therefore not
reachable in image transfer mode.

### What to tell a user

Image transfer mode is a fallback for a photograph. Image transfer mode is not
a fallback for a file.

An earlier note in this project called image mode a working fallback. The note
came from a storage test, and the storage test cannot see the difference.
