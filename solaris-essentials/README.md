# Solaris Essentials

**Deployment: Server-only.** Commands work from an ordinary vanilla client: bounded durable homes and warps/spawn, same-dimension `/back`, tick-timed `/tpa` + `/tpaccept`, and `/msg` + `/reply`.

Names are lowercase letters, digits, `_`, or `-`. `/setspawn`, `/setwarp`, and `/delwarp` require a Solaris operator. `/home` and `/sethome` require the `solaris.essentials.use` node, and `/setwarp`/`/delwarp` require `solaris.essentials.manage`, whenever the deployment provides a permission provider (`solaris-permissions`); the operator check still applies afterwards. `/back` remembers deaths and successful teleports initiated by this plugin; component API 0.7 does not publish arbitrary teleport/movement history. Teleports are same-dimension because the public API has no dimension argument. TPA and private-message name lookup includes only currently online players and rejects ambiguous names.
