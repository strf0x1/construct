# Dropping a peer on another machine

A peer is a construct binary on another box that dials your hub, runs an
agent there, and takes its prompts from you. This page is the first real
drop: Tailscale or a LAN first, a Cloudflare Tunnel when the box is outside
your network. The design is in ADR 0068 (the wire), 0070 (joining), 0071
(sessions), 0072 (model calls through the hub) and 0073 (stamped binaries).

No lane needs a TLS certificate for security: the link is Noise IK end to
end, with the hub's key stamped into the peer and the peer's key pinned on
first join. TLS (`wss://`) is only there when something in the middle, like
a tunnel, wants it.

## Tailscale or LAN (the homelab lane)

Both machines on one tailnet (or one LAN). On the machine that holds your
API keys:

**1. Run the hub on the tailnet address.**

```sh
construct hub --listen tailscale          # this machine's tailnet IP, port 9443
```

`tailscale:<port>` picks another port. On a LAN give the address yourself:
`--listen 192.168.1.20:9443`. `--listen 0.0.0.0:9443` listens everywhere,
which is rarely what you want on a laptop.

**2. Stamp a binary for the peer and invite it.**

```sh
construct build --name nas --tailscale --target x86_64-unknown-linux-musl --invite
```

`--tailscale` fills `--url ws://<this machine's tailnet IP>:<the running
hub's port>`; on a LAN pass `--url ws://192.168.1.20:9443` instead. A
`--target` other than this machine's downloads the published bare binary
for this version (`construct-<target>-bare`, checked against its SHA-256 and
cached in `$CONSTRUCT_HOME/cache/release/`); `aarch64-unknown-linux-musl` for
a Raspberry Pi or an ARM box. The stamp carries the hub's URL and public key,
the peer id and `llm = "hub"`, so the peer needs no API key. Add `--sandbox
readonly-host` for a box you only want to look at (it reads `/etc`,
`/var/log`, … and asks before any write). It prints:

```
invite for nas: single use. Copy the binary over, then on the target run:

  CONSTRUCT_JOIN=… ./construct-nas
```

**3. Copy it over and run it.**

```sh
scp construct-nas nas:
ssh nas 'CONSTRUCT_JOIN=… ./construct-nas'
```

The stamped binary with no arguments is `construct peer`. It pins the hub's
key, the hub pins its key, and the token is spent. From then on it rejoins
without one. Keep it running with `nohup`, `tmux` or a service unit
(`--install` is #367).

Then, on the hub machine:

```sh
construct hub status                      # nas  pinned  online
construct hub prompt nas --wait what is using the most disk here
construct hub session nas                 # the transcript, and any question it is asking
construct hub answer nas once             # answer a permission question
construct hub revoke nas                  # the peer deletes its key and stops
```

## Outside your network: a Cloudflare Tunnel

For a box that is not on your tailnet and cannot reach your LAN, put a
tunnel in front of the hub. The hub keeps listening on loopback; the tunnel
terminates TLS and the peer dials `wss://`. Noise still runs inside, so the
tunnel sees only ciphertext.

**A quick tunnel** (no account, no domain; the URL changes every run, so
it suits a one-off rescue):

```sh
construct hub                                          # loopback, 127.0.0.1:9443
cloudflared tunnel --url http://127.0.0.1:9443         # prints https://<words>.trycloudflare.com
construct build --name rescue --url wss://<words>.trycloudflare.com \
  --target x86_64-unknown-linux-musl --invite
```

**A named tunnel** for a peer that stays: create one with
`cloudflared tunnel create`, route a hostname you own to
`http://127.0.0.1:9443`, and stamp `--url wss://peer-hub.example.com`.
**Tailscale Funnel** does the same with no domain:
`tailscale funnel 9443` publishes `https://<machine>.<tailnet>.ts.net`, and
the peer dials `wss://<machine>.<tailnet>.ts.net`.

Anything that forwards WebSockets works: the hub does not check the path or
the `Host` header.

## Idle links, sleep and wake

Both ends send an encrypted ping every 25 s and drop the link when one goes
unanswered for 10 s, so a link through a NAT or tunnel does not go quiet. A
joined peer never exits on a lost link: it redials with backoff (up to 15 s
between tries), keeps its sessions and running turns, and the hub reattaches
with `session/load`. A prompt the hub sent but never saw answered is resent
with its idempotency key, so it runs once. When the hub's laptop sleeps,
the peer notices within about 35 s, keeps trying, and is back within one
backoff step of the hub waking.

## When it does not connect

- `construct hub status` says where the hub listens. `127.0.0.1` means only
  this machine can reach it; restart it with `--listen tailscale`.
- On macOS the firewall may ask to allow incoming connections for
  `construct` the first time it listens on a non-loopback address.
- `Tailscale is NeedsLogin` (or `Stopped`): run `tailscale up`. Without the
  `tailscale` CLI, give `--listen` and `--url` yourself.
- The peer says the hub refused or denied it: the token expired (15 minutes
  by default; `--expires 2h`), was used already, or the id is wrong. Run
  `construct hub invite <id>` for a new one.
- A stamped binary that will not start and names its stamp was cut or
  changed in transit; copy it again (ADR 0073).
