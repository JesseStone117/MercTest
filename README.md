# Merc Movement Prototype

Small server-authoritative multiplayer movement prototype.

## Run

Install dependencies once:

```bash
npm install
```

Start the Rust server:

```bash
npm run server
```

Start the browser client in another terminal:

```bash
npm run dev
```

Open `http://127.0.0.1:5173`, create a lobby, copy the 4 digit code, and join from another tab.

## Architecture

- Clients send intent only: create lobby, join lobby, start game, move to point.
- The Rust server owns lobby state, spawn positions, movement, facing, and map bounds.
- The browser renders the latest server state with interpolation so movement stays smooth.
- Right click the square map to move.
