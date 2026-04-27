use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, Mutex};

const MAP_HALF_SIZE: f32 = 10.0;
const PLAYER_SPEED: f32 = 5.8;
const TICK_SECONDS: f32 = 1.0 / 30.0;

#[tokio::main]
async fn main() {
    let state = Arc::new(Mutex::new(ServerState::default()));
    spawn_simulation(state.clone());

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let address = SocketAddr::from(([127, 0, 0, 1], 4000));
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();

    println!("server listening on ws://{address}/ws");
    axum::serve(listener, app).await.unwrap();
}

type SharedState = Arc<Mutex<ServerState>>;
type Outbox = mpsc::UnboundedSender<ServerMessage>;

#[derive(Default)]
struct ServerState {
    lobbies: HashMap<String, Lobby>,
    tick: u64,
}

struct Lobby {
    host_id: String,
    started: bool,
    players: HashMap<String, Player>,
    connections: HashMap<String, Outbox>,
}

#[derive(Clone)]
struct Player {
    id: String,
    name: String,
    mercenary_id: String,
    position: Vec2,
    target: Vec2,
    facing: f32,
    moving: bool,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct Vec2 {
    x: f32,
    z: f32,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    CreateLobby {
        name: String,
        #[serde(rename = "mercenaryId")]
        mercenary_id: String,
    },
    JoinLobby {
        code: String,
        name: String,
        #[serde(rename = "mercenaryId")]
        mercenary_id: String,
    },
    StartGame,
    MoveTo {
        x: f32,
        z: f32,
    },
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    LobbyJoined {
        code: String,
        #[serde(rename = "playerId")]
        player_id: String,
        #[serde(rename = "hostId")]
        host_id: String,
        started: bool,
        players: Vec<PlayerView>,
    },
    LobbyUpdate {
        code: String,
        #[serde(rename = "hostId")]
        host_id: String,
        started: bool,
        players: Vec<PlayerView>,
    },
    GameStarted {
        players: Vec<PlayerView>,
    },
    State {
        tick: u64,
        players: Vec<PlayerView>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Serialize)]
struct PlayerView {
    id: String,
    name: String,
    #[serde(rename = "mercenaryId")]
    mercenary_id: String,
    x: f32,
    z: f32,
    facing: f32,
    moving: bool,
}

struct Session {
    lobby_code: String,
    player_id: String,
}

async fn ws_handler(upgrade: WebSocketUpgrade, State(state): State<SharedState>) -> impl IntoResponse {
    upgrade.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: SharedState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    let write_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let Ok(text) = serde_json::to_string(&message) else {
                continue;
            };

            if ws_sender.send(Message::Text(text)).await.is_err() {
                return;
            }
        }
    });

    let mut session: Option<Session> = None;

    while let Some(Ok(message)) = ws_receiver.next().await {
        let Message::Text(text) = message else {
            continue;
        };

        let Ok(client_message) = serde_json::from_str::<ClientMessage>(&text) else {
            send_error(&tx, "Invalid message.");
            continue;
        };

        handle_client_message(&state, &tx, &mut session, client_message).await;
    }

    if let Some(session) = session {
        disconnect_player(&state, &session).await;
    }

    write_task.abort();
}

async fn handle_client_message(
    state: &SharedState,
    tx: &Outbox,
    session: &mut Option<Session>,
    message: ClientMessage,
) {
    match message {
        ClientMessage::CreateLobby { name, mercenary_id } => {
            create_lobby(state, tx, session, name, mercenary_id).await;
        }
        ClientMessage::JoinLobby {
            code,
            name,
            mercenary_id,
        } => {
            join_lobby(state, tx, session, code, name, mercenary_id).await;
        }
        ClientMessage::StartGame => {
            start_game(state, session, tx).await;
        }
        ClientMessage::MoveTo { x, z } => {
            set_move_target(state, session, tx, x, z).await;
        }
    }
}

async fn create_lobby(
    state: &SharedState,
    tx: &Outbox,
    session: &mut Option<Session>,
    name: String,
    mercenary_id: String,
) {
    if session.is_some() {
        send_error(tx, "You are already in a lobby.");
        return;
    }

    let mut state = state.lock().await;
    let code = make_lobby_code(&state);
    let player = make_player(name, mercenary_id);

    let mut lobby = Lobby {
        host_id: player.id.clone(),
        started: false,
        players: HashMap::new(),
        connections: HashMap::new(),
    };

    let player_id = player.id.clone();
    lobby.players.insert(player_id.clone(), player);
    lobby.connections.insert(player_id.clone(), tx.clone());
    state.lobbies.insert(code.clone(), lobby);

    *session = Some(Session {
        lobby_code: code.clone(),
        player_id: player_id.clone(),
    });

    let lobby = state.lobbies.get(&code).unwrap();
    send(
        tx,
        ServerMessage::LobbyJoined {
            code,
            player_id,
            host_id: lobby.host_id.clone(),
            started: lobby.started,
            players: player_views(lobby),
        },
    );
}

async fn join_lobby(
    state: &SharedState,
    tx: &Outbox,
    session: &mut Option<Session>,
    code: String,
    name: String,
    mercenary_id: String,
) {
    if session.is_some() {
        send_error(tx, "You are already in a lobby.");
        return;
    }

    let code = code.trim().to_string();

    if code.len() != 4 {
        send_error(tx, "Enter a 4 digit lobby code.");
        return;
    }

    let mut state = state.lock().await;
    let Some(lobby) = state.lobbies.get_mut(&code) else {
        send_error(tx, "Lobby not found.");
        return;
    };

    if lobby.started {
        send_error(tx, "That game has already started.");
        return;
    }

    let player = make_player(name, mercenary_id);
    let player_id = player.id.clone();

    lobby.players.insert(player_id.clone(), player);
    lobby.connections.insert(player_id.clone(), tx.clone());

    *session = Some(Session {
        lobby_code: code.clone(),
        player_id: player_id.clone(),
    });

    send(
        tx,
        ServerMessage::LobbyJoined {
            code: code.clone(),
            player_id,
            host_id: lobby.host_id.clone(),
            started: lobby.started,
            players: player_views(lobby),
        },
    );

    broadcast_lobby_update(&code, lobby);
}

async fn start_game(state: &SharedState, session: &Option<Session>, tx: &Outbox) {
    let Some(session) = session else {
        send_error(tx, "Create or join a lobby first.");
        return;
    };

    let mut state = state.lock().await;
    let Some(lobby) = state.lobbies.get_mut(&session.lobby_code) else {
        send_error(tx, "Lobby no longer exists.");
        return;
    };

    if lobby.host_id != session.player_id {
        send_error(tx, "Only the host can start the game.");
        return;
    }

    if lobby.started {
        return;
    }

    lobby.started = true;
    spawn_players(lobby);

    let message = ServerMessage::GameStarted {
        players: player_views(lobby),
    };

    broadcast(lobby, message);
}

async fn set_move_target(
    state: &SharedState,
    session: &Option<Session>,
    tx: &Outbox,
    x: f32,
    z: f32,
) {
    let Some(session) = session else {
        send_error(tx, "Create or join a lobby first.");
        return;
    };

    let mut state = state.lock().await;
    let Some(lobby) = state.lobbies.get_mut(&session.lobby_code) else {
        send_error(tx, "Lobby no longer exists.");
        return;
    };

    if !lobby.started {
        send_error(tx, "The game has not started yet.");
        return;
    }

    let Some(player) = lobby.players.get_mut(&session.player_id) else {
        send_error(tx, "Player not found.");
        return;
    };

    player.target = Vec2 {
        x: clamp(x, -MAP_HALF_SIZE, MAP_HALF_SIZE),
        z: clamp(z, -MAP_HALF_SIZE, MAP_HALF_SIZE),
    };
    player.moving = true;
}

async fn disconnect_player(state: &SharedState, session: &Session) {
    let mut state = state.lock().await;
    let Some(lobby) = state.lobbies.get_mut(&session.lobby_code) else {
        return;
    };

    lobby.players.remove(&session.player_id);
    lobby.connections.remove(&session.player_id);

    if lobby.players.is_empty() {
        state.lobbies.remove(&session.lobby_code);
        return;
    }

    if lobby.host_id == session.player_id {
        if let Some(next_host_id) = lobby.players.keys().next() {
            lobby.host_id = next_host_id.clone();
        }
    }

    broadcast_lobby_update(&session.lobby_code, lobby);
}

fn spawn_simulation(state: SharedState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs_f32(TICK_SECONDS));

        loop {
            interval.tick().await;
            let outgoing_messages = tick_lobbies(&state).await;

            for (tx, message) in outgoing_messages {
                send(&tx, message);
            }
        }
    });
}

async fn tick_lobbies(state: &SharedState) -> Vec<(Outbox, ServerMessage)> {
    let mut state = state.lock().await;
    state.tick += 1;

    let tick = state.tick;
    let mut outgoing_messages = Vec::new();

    for lobby in state.lobbies.values_mut() {
        if !lobby.started {
            continue;
        }

        for player in lobby.players.values_mut() {
            move_player(player);
        }

        let message = ServerMessage::State {
            tick,
            players: player_views(lobby),
        };

        for tx in lobby.connections.values() {
            outgoing_messages.push((tx.clone(), message.clone()));
        }
    }

    outgoing_messages
}

fn move_player(player: &mut Player) {
    if !player.moving {
        return;
    }

    let dx = player.target.x - player.position.x;
    let dz = player.target.z - player.position.z;
    let distance = (dx * dx + dz * dz).sqrt();

    if distance <= 0.02 {
        player.position = player.target;
        player.moving = false;
        return;
    }

    player.facing = dx.atan2(dz);

    let step = PLAYER_SPEED * TICK_SECONDS;

    if distance <= step {
        player.position = player.target;
        player.moving = false;
        return;
    }

    player.position.x += (dx / distance) * step;
    player.position.z += (dz / distance) * step;
}

fn spawn_players(lobby: &mut Lobby) {
    let spawns = [
        Vec2 { x: -3.0, z: -3.0 },
        Vec2 { x: 3.0, z: -3.0 },
        Vec2 { x: -3.0, z: 3.0 },
        Vec2 { x: 3.0, z: 3.0 },
        Vec2 { x: 0.0, z: 0.0 },
    ];

    for (index, player) in lobby.players.values_mut().enumerate() {
        let spawn = spawns[index % spawns.len()];
        player.position = spawn;
        player.target = spawn;
        player.facing = 0.0;
        player.moving = false;
    }
}

fn make_player(name: String, mercenary_id: String) -> Player {
    Player {
        id: make_player_id(),
        name: clean_name(name),
        mercenary_id: clean_mercenary_id(mercenary_id),
        position: Vec2 { x: 0.0, z: 0.0 },
        target: Vec2 { x: 0.0, z: 0.0 },
        facing: 0.0,
        moving: false,
    }
}

fn make_lobby_code(state: &ServerState) -> String {
    loop {
        let code = format!("{:04}", rand::thread_rng().gen_range(0..10_000));

        if !state.lobbies.contains_key(&code) {
            return code;
        }
    }
}

fn make_player_id() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect()
}

fn clean_name(name: String) -> String {
    let name = name.trim();

    if name.is_empty() {
        return "Player".to_string();
    }

    name.chars().take(16).collect()
}

fn clean_mercenary_id(mercenary_id: String) -> String {
    if mercenary_id == "welstoce" {
        return mercenary_id;
    }

    "polilock".to_string()
}

fn player_views(lobby: &Lobby) -> Vec<PlayerView> {
    let mut players: Vec<PlayerView> = lobby
        .players
        .values()
        .map(|player| PlayerView {
            id: player.id.clone(),
            name: player.name.clone(),
            mercenary_id: player.mercenary_id.clone(),
            x: player.position.x,
            z: player.position.z,
            facing: player.facing,
            moving: player.moving,
        })
        .collect();

    players.sort_by(|a, b| a.name.cmp(&b.name));
    players
}

fn broadcast_lobby_update(code: &str, lobby: &Lobby) {
    let message = ServerMessage::LobbyUpdate {
        code: code.to_string(),
        host_id: lobby.host_id.clone(),
        started: lobby.started,
        players: player_views(lobby),
    };

    broadcast(lobby, message);
}

fn broadcast(lobby: &Lobby, message: ServerMessage) {
    for tx in lobby.connections.values() {
        send(tx, message.clone());
    }
}

fn send(tx: &Outbox, message: ServerMessage) {
    let _ = tx.send(message);
}

fn send_error(tx: &Outbox, message: &str) {
    send(
        tx,
        ServerMessage::Error {
            message: message.to_string(),
        },
    );
}

fn clamp(value: f32, min: f32, max: f32) -> f32 {
    value.max(min).min(max)
}
