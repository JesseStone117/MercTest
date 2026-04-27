# Merc Movement Prototype

Small server-authoritative multiplayer movement prototype.

## Run

Install dependencies once:

```bash
npm install
```

Start the app:

```powershell
.\powershell-commands.ps1 dev
```

Open `http://localhost:4000` on this computer.

For another computer or phone on the same network, run `ipconfig`, find your IPv4 address, and open:

```text
http://YOUR_IPV4_ADDRESS:4000
```

The client connects back to `ws://YOUR_IPV4_ADDRESS:4000/ws`, so port `4000` needs to be allowed through the Windows firewall.

## Architecture

- Clients send intent only: create lobby, join lobby, start game, move to point.
- The Rust server owns lobby state, spawn positions, movement, facing, and map bounds.
- The browser renders the latest server state with interpolation so movement stays smooth.
- Right click the square map to move.
