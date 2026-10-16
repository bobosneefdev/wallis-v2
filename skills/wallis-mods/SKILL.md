---
name: wallis-mods
description: Write and hot-load Discord slash commands using TypeScript and Effect v4.
---
Every feature is a mod in /var/lib/wallis/mods/<name>/{mod.json,index.ts}. Read the stock mods and _lib/wallis.ts first. Use Effect v4 for all new TypeScript, compose with Effect.gen, and use the effect skill. Import input, emit, reply, runMod from ../_lib/wallis.ts. Read input once. emit(tool,args) returns the host result. Finish with reply. Never print logs to stdout; stderr is available.

mod.json: {"name":"coinflip","description":"Flip a coin","options":[]}. Names and option names match [a-z0-9-]{1,32}; profile is reserved. Descriptions are 1–100 characters. Up to 25 unique options with type string/integer/boolean and required true/false; required options come first. Never write enabled in the manifest. Desktop settings alone control enabled state. Never re-create a deleted mod unless asked.

Available actions: message(channelId,text), file(channelId,filename,content,text), queue(path,title), queue_list, skip_audio, stop_audio, volume(percent 0–100), delegate(task,context,channelId), reply(text). Input contains options, user, guild, channel (Discord IDs as strings). Use only the supplied channel. Media paths must be inside /var/lib/wallis/media, files at most 50 MiB.

Before finishing run /opt/wallis/tools/bun/bun run check from mods/. Add a small runnable check for non-trivial logic. The host rescans commands after the job. Fix validation errors before reporting success.

Write only mods/, worker-scratch/ and media/. The shared workspace budget is 3 GiB including node_modules. Prefer installed tools, never global installs or package managers outside mods/. Delete temporary files, partial downloads and browser profiles. Report the size of anything installed. No collections of downloaded music or browser caches. You cannot edit the Rust host or read the Discord token, realtime memory or Windows files.
