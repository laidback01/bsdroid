# bsdroid

Mount an Android cellphone as a folder, on FreeBSD.

## Why this exists

My phone, a Samsung Galaxy S22, didn't work well with the existing tools on FreeBSD 15.1. 
I really don't want to use a specialized application for my phone, the phone should just
be another mounted filesystem. I should be able to browse the phone with whatever 
filemanager is in my window manager at the time.  I do rotate between Gnome, KDE, XFCE, etc
as time allows. Having fusefs mount my phone seems like such a simple solution. None of
the exisiting solutions worked for me. Mount point would give me some odd usb error,
and one of my cores would max out at 100%. 

I've used jmtpfs, simple-mtpfs, and tried aft-mtp-mount. On the day this
project started, `simple-mtpfs` left a mount point in this state:

```
d---------   0 root wheel  0 Dec 31  1969 phone
```

well... that's great.

To be VERY CLEAR: I'm not a developer, I'm mostly a system admin who breaks
things, sometimes fixes them. I leave the development to the real coders, or in
this case AI! This code is 100% AI written, if you don't like that, oh well.
It's good code so far as a simple admin can tell. Famous last words: it works
for me!

### The state of things

FreeBSD ports hold three other programs that mount a cellphone over MTP. Each
one ran against a Samsung SM-S901U on the same host, with the same files:

| Program         | Mounts | Lists a folder | 290 KB | 8 MB | 450 MB |
| --------------- | ------ | -------------- | ------ | ---- | ------ |
| `simple-mtpfs`  | no     | no             | no     | no   | no     |
| `jmtpfs`        | no     | no             | no     | no   | no     |
| `aft-mtp-mount` | yes    | yes            | yes    | yes  | no     |

These are good programs, and each one works on Linux. The fault is not in them.

Each program reaches the cellphone through `/usr/lib/libusb.so.3`. FreeBSD has
no native `libusb-1.0`, so that file is a compatibility layer over the native
USB library. The layer holds a defect. When a transfer stalls, the layer polls
with no timeout, and the layer never gives control back. A program that uses
the layer cannot set a deadline, and cannot recover.

`docs/00-why.md` holds the measurement: 455262 system calls in 20 seconds, with
106 useful transfers among them.

### The choice this project made

There are two roads. Repair the compatibility layer, or do not use the layer.

This project does not use the layer. FreeBSD ships `libusb20` in the base
system, which is the native USB library, and which gives a deadline that the
caller controls. The USB code and the MTP code here are new.

That is not a criticism of the other programs. A repair of the compatibility
layer affects every program on FreeBSD which uses it... The repair is still
worth doing, but by someone who would be aware of what all this shim affects.
This project took the other road just because I didn't want to try and wrangle
that!

### Why not adb, or one of the wireless things?

Every time I post this, someone suggests a different tool. They are all decent
suggestions and most of them solve a different problem than mine.

**adb.** Works well, and I use it in the tests here as the reference: a read
over MTP is compared against a copy over `adb`, by SHA-256 sum. Two things it
does not do for me. It needs developer options turned on, which I do not want
standing on for a phone that just holds photos, and it is not a filesystem, so
Nautilus cannot see it. You can build a shell around it, and then you have
built this, with an extra thing enabled on the phone.

**Immich, Syncthing, PhotoSync, KDEConnect, photoprism.** These are continuous
sync. They are good at that, and if you want your photos to arrive on a server
by themselves, use one of them. They need a server, or an account, or the
network. I wanted to plug a cable in and look at a folder. I don't always have 
a network on a phone. Literally have a phone without a sim and the wifi is bad. 
But it's camera is really nice, and it's convenient. So I still want to get 
data off of it.

**Copy to a USB drive first.** Yeah, cool. Pain in the butt for me though. Seems
like this is a suggestion that covers most of my needs, but then I need a file
browser app on my phone. Huh... I've got cables galore. Going to use them.

This software is one job: connect a cellphone, and browse it in the file manager
you already run. Nothing special on the phone, no dev options, just turn on 
file sharing when it's connected via the cable. FUSE is how you get that on FreeBSD, 
and it works the same under Nautilus, Dolphin or Thunar. For me, this is great!

### "MTP is slow and fragile"

Slow: I don't have the right cable it seems, and can't get beyond 42.7 MiB/s, 
but that's pretty good on High Speed. The link is the limit, not the protocol. 
See `docs/04-file-transfer.md`.

Fragile: that reputation is earned, but not by MTP. Two things earn it. The
first is the compatibility layer above, which cannot set a deadline. The
second is a list of about a dozen device habits that a host must handle, and
which I have not found written down anywhere else:

- a storage list that is empty for the first moment after a connect
- `GetObjectHandles`, where 0x00000000 and 0xffffffff swap meaning by device
- a packet of zero bytes, needed when a data phase ends on a packet boundary
- a cellphone that drops the last byte when a partial read ends on 512
- a session that an earlier program left open
- an endpoint that needs a drain before the first command
- MTP behind a vendor class, which you can only name by reading a string
- a fast folder listing that some devices refuse
- a cellphone on the bus one or two seconds before its MTP interface is
- charge only mode, which looks the same as file transfer until you ask for
  the storage

Handle those and MTP is neither slow nor fragile. Miss one and it looks like
both. The files in `docs/` hold the measurement for each.

I did some chaos testing with 3 phones, three connected writes, pulling cables,
resetting, rotating ports, iterating the usb mode, etc. I've got a known bad cable
I used as well. Took a bit, but we have a system that found most of the 
issues that crop up under that load, and have modest solution here.
583 writes of 24 MiB went out and came back with the same bytes, no command
failed to return, and nothing was lost in silence. 
See `docs/10-what-works.md`.

### Does this use libmtp?

No. This project holds its own MTP code, in `crates/ptp-proto`.

The project read the device fault database of `libmtp` as reference material,
and `docs/08-what-libmtp-knows.md` compares the two. The project links no part
of `libmtp`, and calls no function of it.

Linker shows we are using that shim, but it's not accurate:

```
ldd target/release/mtpfs
        libusb.so.3 => /usr/lib/libusb.so.3
```

That file looks like the compatibility layer. FreeBSD puts two interfaces in
one file: the `libusb-1.0` compatibility layer, and the native `libusb20`.
There is no separate file for the native one.

The symbols show which interface a program uses:

```
$ nm -u target/release/mtpfs | grep -c libusb20_
15
$ nm -u target/release/mtpfs | grep 'U libusb_' | grep -v libusb20
(nothing)
```

Fifteen calls to the native interface. None to the compatibility layer.

### Then how does this work for a device I do not own?

`libmtp` holds a table of 1529 devices, and a reader can fairly ask whether
this project needs the table.

Of the 1529 entries, 833 are Android, and each of the 833 carries the same six
flags. The table is therefore one rule for an Android device, and not 833
different faults.

This project does the safe thing for each device, in place of a table:

- The project does not use the three operations that the flags call broken.
- The project takes the interface from the kernel, for each device.
- The project sends no USB reset, because a reset breaks some devices.
- The project works around the Samsung read fault for each device, because the
  workaround costs nothing on a device with no fault.

A link to `libmtp` also brings back `libusb-1.0`, and that layer holds the
defect this project exists to avoid.

The real risk is not a device quirk. The risk is the search for the MTP
interface, which two of three test devices do one way and the third does
another. A report from a device this project does not find is the most useful
thing you can send. See `docs/08-what-libmtp-knows.md`.

## What works now

| Operation              | State |
| ---------------------- | ----- |
| Mount                  | yes   |
| List a folder          | yes   |
| Read a file            | yes   |
| Write a new file       | yes   |
| Make a folder          | yes   |
| Remove a file          | yes   |
| Remove a folder        | yes   |
| Rename a file          | yes   |
| Move a file            | yes   |
| Report the free space  | yes   |
| Overwrite a file       | yes   |
| Append to a file       | yes   |
| Truncate a file        | yes   |
| Change a file in place | yes   |

A read gives the same bytes as a copy over `adb`. A test compares a SHA-256
sum, for a file of 290 KB and for a file of 450 MB.

A write streams through a spool file on disk, so memory stays flat. A copy of a
649 MB file uses about 6 MB of memory. Set `BSDROID_SPOOL` to choose the folder
for the spool file; the default is `/var/tmp`.

`cp -p` works. MTP holds no time, no mode and no owner, so the filesystem
accepts each request and changes nothing. A fault there stops a copy, and a
copy is the job.

### Changing a file that is already on the phone

This works. `cp` onto an existing file, `>>`, `truncate`, and `sed -i` all do
what you expect, and a file manager's "Replace" prompt does too.

MTP has no operation that writes into the middle of an object, and no operation
that renames one object onto the name of another, so a change becomes a new
object. The steps follow copy-on-write: read the file into a spool file on your
disk, let the program change it, move the old object aside, send the new
contents under the real name, then delete the old object. The old contents
always survive until the new contents hold the real name.

ZFS makes the last two steps one atomic step. MTP gives no atomic step, so the
promise here is smaller: nothing is destroyed before the replacement is in
place, but a mount that dies mid-swap can leave the new file plus an
`.bsdroid-old-N` beside it. You see both, and you lose nothing.

Two costs are worth knowing before you edit a large file:

- Your disk needs room for the whole file, because the spool file holds it.
  Changing one byte of a 4 GB file reads 4 GB and writes 4 GB.
- The phone needs room for two copies while the swap happens. If it cannot
  hold two, the old object is deleted first and the spool file is the only
  copy until the send finishes. The log says when it takes that route, and if
  that send fails it keeps the spool file and prints its path.

If neither route fits, you get `ENOSPC` and nothing is touched. Set
`BSDROID_SPOOL` to put the spool file somewhere with more room.

`docs/10-what-works.md` holds the full list, with each limit and the reason.

## Use a good cable. Seriously.

Before you blame this program, or your cellphone, swap the cable.

One of the three test cellphones could not write a file. Reads were perfect,
listings were perfect, and writes failed at random. Not by size, not by timing,
not after any particular idle period - just at random. I spent a long evening
on it. I raised the transfer deadline to 60 seconds. I matched the write size
to the buffer in the Android kernel driver. I sent the USB reset that `libmtp`
recommends for that exact chip. I power cycled the phone. Nothing moved the
number.

Then the phone itself told us, once USB debugging was on:

```
E d.process.medi: Mtp got unexpected short packet
E MtpServer: Mtp receive file got error I/O error
W MtpServer: [MTP] got response 0x2002 in command MTP_OPERATION_SEND_OBJECT
```

A short packet where none belongs. That is what a marginal cable looks like
from the other end of the wire.

Same phone, same file, same test, ten writes of 1 MB each:

| Cable     | Writes that worked |
| --------- | ------------------ |
| The old one | 0 of 10          |
| A new one   | 10 of 10         |

Nothing else changed. Not one line of code. The new cable gives 10 of 10 with
developer options on, and 10 of 10 with developer options off, so this is not
some debug setting doing the work.

The cable still charged the phone. It still enumerated at full USB 2.0 speed,
480 Mbps. It read files perfectly, including a 16 MB file five times over with
matching SHA-256 sums every time. It just could not carry a write.

So: if writes fail and reads are fine, try another cable before you open an
issue. A charging cable that came free with something is the usual suspect.

## Build

```
pkg install fusefs-libs3
cargo build --release
```

### Install

For your own account, with no root:

```
mkdir -p ~/.local/bin
install -m 755 target/release/mtpfs ~/.local/bin/
install -m 755 target/release/mtpprobe ~/.local/bin/
```

Put `~/.local/bin` in your `PATH`. For the whole host, use
`/usr/local/bin` instead.

A mount needs `vfs.usermount=1`, and your account needs read and write on the
`ugen` device. Check with `sysctl vfs.usermount`.

## Use

Before you start:

1. Connect the cellphone.
2. Unlock the cellphone.
3. Put the cellphone into file transfer mode.

### List each cellphone the host sees

```
$ mtpfs -l
NODE          ID            NAME
ugen0.11      04e8:6860    SAMSUNG SAMSUNG_Android
ugen0.12      22b8:2e82    motorola Moto G (5)
```

### Mount

The form follows `mount_msdosfs`: the device, and then the folder.

```
mtpfs ugen0.11 /mnt/phone
```

A node name also takes the full form, `/dev/ugen0.11`.

A node name is not stable. FreeBSD gives the address when a cellphone
attaches, so a cellphone that leaves the bus and comes back can take the node
another cellphone had. Name the cellphone by its identifiers when the answer
must stay right:

```
mtpfs 04e8:6860 /mnt/phone
```

`mtpfs -l` prints both names.

The product changes with the USB mode of the cellphone, so a pair of
identifiers names one cellphone in one mode. See `docs/02-device-states.md`.

With no device, the program takes the first cellphone it finds:

```
mtpfs /mnt/phone
```

### Mount two cellphones

Each mount names one device, so two mounts hold two cellphones:

```
mtpfs ugen0.11 /mnt/samsung
mtpfs ugen0.12 /mnt/moto
cp /mnt/samsung/DCIM/Camera/a.jpg /mnt/moto/DCIM/Camera/
```

A copy between two cellphones goes through the host, and the bytes arrive. A
test compares a SHA-256 sum.

### Stop a mount

```
umount /mnt/phone
```

Use `umount`. A signal to the program leaves the session open on the cellphone,
and the next mount then needs a repair.

### Options

An option goes to FUSE:

```
mtpfs ugen0.11 /mnt/phone -f              stay in the foreground, and write messages
mtpfs ugen0.11 /mnt/phone -d              stay in the foreground, and write each request
mtpfs ugen0.11 /mnt/phone -o allow_other  give a mount option to FUSE
```

The value after `-o` belongs to the option, and not to the device.

### The probe

`mtpprobe` reports what a cellphone does, and where a transfer stops. Run
`mtpprobe --help` for the commands.

With no `-d`, each command takes the first cellphone it finds. Name a device
to reach another one:

```
mtpprobe -d ugen0.12 probe
mtpprobe -d ugen0.12 caps
```

## Test hardware

Three cellphones, from three makers, with two chip makers:

| Cellphone           | Sold as    | Chip     |
| ------------------- | ---------- | -------- |
| Samsung SM-S901U    | Galaxy S22 | Qualcomm |
| Motorola Moto G (5) | Moto G5    | Qualcomm |
| Cyrus CS 24         | NUU B20    | MediaTek |

The three do not agree about much. Two use the standard USB class for MTP, and
one uses a vendor class. One waits before it reports a storage, and two do not.
Each one gives a different name to the same USB mode.

Each disagreement broke a rule that came from one cellphone. The files in
`docs/` hold the measurement for each one.

A fourth cellphone is more use to this project than a hundred more runs on
these three:

```
cargo build
sh tools/capture-device.sh <a name for your device>
```

The script hides each file name. Read the file in `docs/captures` before you
send the file.

### Checks that need a cellphone

Two faults in this project could not be found by a test, because a test cannot
make hardware leave the bus. These two checks can.

```
sh tools/check-write-errors.sh /mnt/phone ugen0.11
```

Asks one question: does a write that fails reach the program that wrote it?
The check needs no cable in anybody's hand, and it moves no bytes. It takes ten
seconds. `mtpfs` once reported success for a file that never left the host. See
`docs/07-filesystem-design.md`.

```
sh tools/chaos-cable.sh /mnt/phone 04e8:6860 write 600
```

Needs a person with a cable. Pull the cable at any moment, as often as you
like, and put it back. The check reads or writes the whole time, and it counts
four outcomes:

| Outcome                                      | What it means                   |
| -------------------------------------------- | ------------------------------- |
| a command fails                              | a pass, the cellphone is gone   |
| a command never returns                      | the fault this project avoids   |
| a write holds wrong bytes                    | worse than a fault              |
| a write reports success after a reported one | the program was told a lie      |

### Does changing a file in place work?

```
doas sh tools/check-in-place-edit.sh ./target/debug/mtpfs 04e8:6860 phone /tmp/out
```

Runs 19 checks: overwrite, truncate, append, `sed -i`, the temp-file-and-rename
dance an editor does, and a plain rename. Each one checks the contents
afterwards, not just the exit status, and it fails if the folder is left with
anything unexpected in it. The code before this feature scored 4 of 19; all
three test phones now score 19 of 19.

Run `mtpfs -l` for the vendor and the product of a cellphone.

## How this was built

Claude Code and I wrote this together, over one long session. I supplied the
cellphones, the cables, the hands, and the questions that broke the wrong
answers... basically, I was the on-site person for a remote coder.

Several documents in `docs/` record a correction. The project measured one
device, drew a rule, met a second device, and found the rule wrong. Some of the
faults were in this project, and each document says so.

## Documentation

| File                               | Subject                                     |
| ---------------------------------- | ------------------------------------------- |
| `docs/00-why.md`                   | The defect in the compatibility layer       |
| `docs/01-cold-start.md`            | A cellphone that reports no storage at first |
| `docs/02-device-states.md`         | What a USB mode changes, and what it does not |
| `docs/03-object-handles.md`        | How a device lists the objects it holds     |
| `docs/04-file-transfer.md`         | The copy of a file, and the rate            |
| `docs/05-finding-the-interface.md` | Two ways a cellphone gives MTP              |
| `docs/06-the-reset-that-breaks.md` | A repair that breaks a working device       |
| `docs/07-filesystem-design.md`     | The design of the filesystem                |
| `docs/08-what-libmtp-knows.md`     | What the `libmtp` database already knew     |
| `docs/09-the-other-tools.md`       | The measurements of the other programs      |
| `docs/10-what-works.md`            | What works, what does not, and what is slow |

The files in `docs/` use Simplified Technical English (ASD-STE100), because
many readers of this project read English as a second language.
`tools/lint-docs.sh` checks each one.

This README is not checked. A reference document needs a plain rule. A person
who tells you why the person wrote a program needs a voice.

## Tests

```
cargo test
```

The tests need no cellphone. The test data comes from real captures, and
`crates/ptp-proto/tests/fixtures/README.md` records the source of each one.

## Will it work with your cellphone?

### What is verified

Three cellphones, and each one reads and writes:

| Cellphone           | Sold as    | Chip     | Android | MTP interface  |
| ------------------- | ---------- | -------- | ------- | -------------- |
| Samsung SM-S901U    | Galaxy S22 | Qualcomm | 16      | 0x06/0x01/0x01 |
| Motorola Moto G (5) | Moto G5    | Qualcomm | 8.1     | 0xff/0xff/0x00 |
| Cyrus CS 24         | NUU B20    | MediaTek | 11      | 0x06/0x01/0x01 |

Three makers, two chip makers, and three versions of Android that are eight
years apart.

### How this project finds a device

A cellphone gives MTP in one of two shapes, and the three above give both:

1. The still imaging class, 0x06/0x01/0x01, which the USB standard defines.
2. A vendor class, 0xff/0xff/0x00, with the interface name `MTP`.

This project reads the descriptor and finds each shape. The project holds no
list of device identifiers.

### How libmtp finds a device, which is not the same

`libmtp` looks in the table of 1529 devices first, by vendor identifier and
product identifier. The comment in the source gives the reason:

```
// First check if we know about the device already.
// Devices well known to us will not have their descriptors
// probed, it caused problems with some devices.
```

A device that is absent from the table gets a second test. `libmtp` reads a
Microsoft descriptor at string index 0xee, and looks for the letters `MSFT`.

The second test needs a vendor class. The test for the still imaging class sits
inside `#if 0` in `libusb1-glue.c`, so the test never runs.

The table is therefore not a list of faults. The table is how `libmtp` finds
most devices.

### What each one finds, and what each one misses

| Shape of the device                    | `libmtp`        | This project |
| -------------------------------------- | --------------- | ------------ |
| Still imaging class, in the table      | yes             | yes          |
| Still imaging class, absent from the table | no          | yes          |
| Vendor class, named `MTP`              | yes             | yes          |
| Vendor class, with a Microsoft descriptor | yes           | **no**       |

The first project has a table with many years of work in it. This project reads
the descriptor, so a new device needs no entry.

The last row is a gap in this project. A device with a vendor class, and an
interface name that is not `MTP`, gives a Microsoft descriptor that `libmtp`
reads and this project does not. No test device of this project needs that
test, so the project has no way to test the code.

### What is not measured

This project does not claim a number. The `libmtp` table records no interface
class, so nobody can count how many of those 1529 devices have which shape.
A count here would be a guess with a decimal point on it.

### If your cellphone does not work

Run the probe. The probe reports what your cellphone does, and where the work
stops:

```
BSDROID_DEBUG=1 mtpprobe probe
```

The output holds one line for each USB interface, with the class, the subclass,
the protocol, the name index and the result of each test. A cellphone with a
third shape shows up there.

For a full report:

```
sh tools/capture-device.sh <a name for your device>
```

The script hides each file name. Read the file in `docs/captures` before you
send the file.

A report from a cellphone this project does not find is the most useful thing
you can send. Three cellphones agreed about the name of the interface. A fourth
that disagrees changes the code.

## Licence

BSD 2-Clause.
