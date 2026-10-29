# simporter — claim simulators and ports before you use them

Several AI agents share this machine. They collide on simulators, emulators and TCP ports.
`simporter` is the broker that prevents it.

1. **Never use a port you were not granted.** Something already listening on 8081 is another
   agent's Metro, not yours.
2. **Never free a port by killing its holder.** No `kill`, no `fuckport`, no `pkill -f metro`.
3. **Never touch a device whose name does not carry the `Agent` suffix.** It belongs to the human.
4. **Release when you finish**, or hold the lease under `simporter run`.

Before you boot or drive a simulator or emulator, start Metro or any server, or connect to a port
you did not start, load the `simporter` skill. If the skill is not available, run
`simporter rule --skill` and follow what it prints.
