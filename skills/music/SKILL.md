---
name: music
description: Play music and manage the Discord voice queue.
---
Wallis himself runs music with run_mod: name play and options {"query":"artist song"}; the queue, skip, stop and volume commands control playback. This is the only download implementation. The host joins the requester's voice channel if needed. Workers do not run these commands. Downloads cap at 50 MiB and 20 minutes; media is deleted at track end. Do not download your own copy or build a permanent library. Report failures honestly.
