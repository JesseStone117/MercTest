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
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, Mutex};
use tower_http::services::{ServeDir, ServeFile};

const MAP_HALF_SIZE: f32 = 10.0;
const POLILOCK_SPEED: f32 = 5.8;
const TICK_SECONDS: f32 = 1.0 / 30.0;
const MAX_HEALTH: f32 = 100.0;
const ATTACK_RANGE: f32 = 1.7;
const ATTACK_DAMAGE: f32 = 10.0;
const ATTACK_SECONDS: f32 = 0.85;
const ATTACK_DAMAGE_POINT_SECONDS: f32 = ATTACK_SECONDS * 0.5;
const RESPAWN_SECONDS: f32 = 3.0;

#[tokio::main]
async fn main() {
    let state = Arc::new(Mutex::new(ServerState::default()));
    spawn_simulation(state.clone());
    let static_root = static_client_root();

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", get(ws_handler))
        .fallback_service(
            ServeDir::new(&static_root)
                .not_found_service(ServeFile::new(static_root.join("index.html"))),
        )
        .with_state(state);

    let port = server_port();
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();

    println!("Static client root: {}", static_root.display());
    println!("Server running at http://localhost:{port}");
    println!("Listening on: {address}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

type SharedState = Arc<Mutex<ServerState>>;
type Outbox = mpsc::UnboundedSender<ServerMessage>;

fn static_client_root() -> PathBuf {
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root_from_repo = current_dir.join("dist");

    if root_from_repo.exists() {
        return root_from_repo;
    }

    current_dir.join("../dist")
}

fn server_port() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(4000)
}

async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_err() {
        return;
    }

    println!("Stopping server...");
}

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
    spawn: Vec2,
    target: Vec2,
    facing: f32,
    health: f32,
    dead: bool,
    moving: bool,
    attacking: bool,
    attack_target_id: Option<String>,
    attack_hit_target_id: Option<String>,
    attack_cooldown: f32,
    attack_timer: f32,
    attack_damage_pending: bool,
    respawn_timer: f32,
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
    TargetEnemy {
        #[serde(rename = "playerId")]
        player_id: String,
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
    health: f32,
    dead: bool,
    moving: bool,
    attacking: bool,
    #[serde(rename = "attackTargetId")]
    attack_target_id: Option<String>,
}

struct Session {
    lobby_code: String,
    player_id: String,
}

struct DamageEvent {
    target_id: String,
    amount: f32,
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
        ClientMessage::TargetEnemy { player_id } => {
            set_attack_target(state, session, tx, player_id).await;
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

    if player.dead {
        send_error(tx, "You cannot move while defeated.");
        return;
    }

    let move_target = Vec2 {
        x: clamp(x, -MAP_HALF_SIZE, MAP_HALF_SIZE),
        z: clamp(z, -MAP_HALF_SIZE, MAP_HALF_SIZE),
    };

    change_move_focus(player, move_target);
    player.attack_target_id = None;
}

async fn set_attack_target(
    state: &SharedState,
    session: &Option<Session>,
    tx: &Outbox,
    target_id: String,
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

    if target_id == session.player_id {
        send_error(tx, "You cannot target yourself.");
        return;
    }

    let target_is_valid = match lobby.players.get(&target_id) {
        Some(target) => !target.dead,
        None => false,
    };

    if !target_is_valid {
        send_error(tx, "Target not found.");
        return;
    }

    let Some(player) = lobby.players.get_mut(&session.player_id) else {
        send_error(tx, "Player not found.");
        return;
    };

    if player.dead {
        send_error(tx, "You cannot attack while defeated.");
        return;
    }

    change_attack_focus(player);
    player.attack_target_id = Some(target_id);
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

        let snapshot = lobby.players.clone();
        let mut damage_events = Vec::new();

        for player in lobby.players.values_mut() {
            if let Some(damage_event) = tick_player(player, &snapshot) {
                damage_events.push(damage_event);
            }
        }

        apply_damage(lobby, damage_events);
        clear_dead_targets(lobby);

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

fn tick_player(player: &mut Player, players: &HashMap<String, Player>) -> Option<DamageEvent> {
    if player.dead {
        tick_respawn(player);
        return None;
    }

    tick_attack_cooldown(player);

    if player.attack_timer > 0.0 {
        return tick_attack_cycle(player, players);
    }

    player.attacking = false;

    let Some(target_id) = player.attack_target_id.clone() else {
        player.moving = move_player_toward(player, player.target, 0.02);
        return None;
    };

    let Some(target) = players.get(&target_id) else {
        stop_player(player);
        return None;
    };

    if target.dead {
        stop_player(player);
        return None;
    }

    face_position(player, target.position);
    let distance = distance_between(player.position, target.position);

    if distance > ATTACK_RANGE {
        player.moving = move_player_toward(player, target.position, ATTACK_RANGE * 0.9);
        return None;
    }

    player.target = player.position;
    player.moving = false;

    if player.attack_cooldown > 0.0 {
        return None;
    }

    start_attack(player, target_id);
    None
}

fn tick_attack_cycle(player: &mut Player, players: &HashMap<String, Player>) -> Option<DamageEvent> {
    player.moving = false;
    player.attacking = true;

    if let Some(target_id) = &player.attack_hit_target_id {
        if let Some(target) = players.get(target_id) {
            face_position(player, target.position);
        }
    }

    player.attack_timer = (player.attack_timer - TICK_SECONDS).max(0.0);
    let reached_damage_point =
        player.attack_damage_pending && player.attack_timer <= ATTACK_SECONDS - ATTACK_DAMAGE_POINT_SECONDS;
    let damage_event = attack_damage_event(player, players);

    if reached_damage_point {
        finish_attack_cycle(player);
        resume_focus_after_attack(player, players);
        return damage_event;
    }

    if player.attack_timer <= 0.0 {
        finish_attack_cycle(player);
        resume_focus_after_attack(player, players);
    }

    damage_event
}

fn attack_damage_event(player: &mut Player, players: &HashMap<String, Player>) -> Option<DamageEvent> {
    if !player.attack_damage_pending {
        return None;
    }

    if player.attack_timer > ATTACK_SECONDS - ATTACK_DAMAGE_POINT_SECONDS {
        return None;
    }

    player.attack_damage_pending = false;
    let Some(target_id) = player.attack_hit_target_id.clone() else {
        return None;
    };

    let Some(target) = players.get(&target_id) else {
        return None;
    };

    if target.dead {
        return None;
    }

    Some(DamageEvent {
        target_id,
        amount: ATTACK_DAMAGE,
    })
}

fn move_player_toward(player: &mut Player, target: Vec2, stop_distance: f32) -> bool {
    let dx = target.x - player.position.x;
    let dz = target.z - player.position.z;
    let distance = (dx * dx + dz * dz).sqrt();

    if distance <= stop_distance {
        if stop_distance <= 0.02 {
            player.position = target;
        }

        return false;
    }

    player.facing = dx.atan2(dz);
    let step = player_speed(player) * TICK_SECONDS;
    let remaining_distance = distance - stop_distance;

    if remaining_distance <= step {
        player.position.x += (dx / distance) * remaining_distance;
        player.position.z += (dz / distance) * remaining_distance;
        return false;
    }

    player.position.x += (dx / distance) * step;
    player.position.z += (dz / distance) * step;

    true
}

fn player_speed(player: &Player) -> f32 {
    if player.mercenary_id == "welstoce" {
        return POLILOCK_SPEED * 1.1;
    }

    POLILOCK_SPEED
}

fn start_attack(player: &mut Player, target_id: String) {
    player.target = player.position;
    player.moving = false;
    player.attacking = true;
    player.attack_cooldown = ATTACK_SECONDS;
    player.attack_timer = ATTACK_SECONDS;
    player.attack_damage_pending = true;
    player.attack_hit_target_id = Some(target_id);
}

fn cancel_attack(player: &mut Player) {
    player.attacking = false;
    player.attack_timer = 0.0;
    player.attack_damage_pending = false;
    player.attack_hit_target_id = None;
}

fn finish_attack_cycle(player: &mut Player) {
    player.attacking = false;
    player.attack_timer = 0.0;
    player.attack_damage_pending = false;
    player.attack_hit_target_id = None;
}

fn change_move_focus(player: &mut Player, target: Vec2) {
    if is_attack_cancelable(player) {
        cancel_attack(player);
    }

    player.target = target;

    if player.attack_timer > 0.0 {
        return;
    }

    player.moving = true;
}

fn change_attack_focus(player: &mut Player) {
    if is_attack_cancelable(player) {
        cancel_attack(player);
    }

    if player.attack_timer > 0.0 {
        return;
    }

    player.moving = true;
    player.attacking = false;
}

fn is_attack_cancelable(player: &Player) -> bool {
    player.attack_timer > ATTACK_SECONDS - ATTACK_DAMAGE_POINT_SECONDS
}

fn resume_focus_after_attack(player: &mut Player, players: &HashMap<String, Player>) {
    let Some(target_id) = player.attack_target_id.clone() else {
        player.moving = move_player_toward(player, player.target, 0.02);
        return;
    };

    let Some(target) = players.get(&target_id) else {
        stop_player(player);
        return;
    };

    if target.dead {
        stop_player(player);
        return;
    }

    face_position(player, target.position);

    if distance_between(player.position, target.position) > ATTACK_RANGE {
        player.moving = move_player_toward(player, target.position, ATTACK_RANGE * 0.9);
        return;
    }

    player.target = player.position;
    player.moving = false;
}

fn tick_attack_cooldown(player: &mut Player) {
    if player.attack_cooldown <= 0.0 {
        return;
    }

    player.attack_cooldown = (player.attack_cooldown - TICK_SECONDS).max(0.0);
}

fn apply_damage(lobby: &mut Lobby, damage_events: Vec<DamageEvent>) {
    for damage_event in damage_events {
        let Some(target) = lobby.players.get_mut(&damage_event.target_id) else {
            continue;
        };

        if target.dead {
            continue;
        }

        target.health = (target.health - damage_event.amount).max(0.0);

        if target.health >= 1.0 {
            continue;
        }

        kill_player(target);
    }
}

fn clear_dead_targets(lobby: &mut Lobby) {
    let dead_ids: Vec<String> = lobby
        .players
        .values()
        .filter(|player| player.dead)
        .map(|player| player.id.clone())
        .collect();

    if dead_ids.is_empty() {
        return;
    }

    for player in lobby.players.values_mut() {
        let Some(target_id) = &player.attack_target_id else {
            continue;
        };

        if dead_ids.contains(target_id) {
            player.attack_target_id = None;

            if player.attack_timer <= 0.0 {
                stop_player(player);
            }
        }
    }
}

fn stop_player(player: &mut Player) {
    player.target = player.position;
    player.moving = false;
    cancel_attack(player);
    player.attack_target_id = None;
}

fn kill_player(player: &mut Player) {
    stop_player(player);
    player.health = 0.0;
    player.dead = true;
    player.respawn_timer = RESPAWN_SECONDS;
}

fn tick_respawn(player: &mut Player) {
    player.respawn_timer = (player.respawn_timer - TICK_SECONDS).max(0.0);

    if player.respawn_timer > 0.0 {
        return;
    }

    respawn_player(player);
}

fn respawn_player(player: &mut Player) {
    player.position = player.spawn;
    player.target = player.spawn;
    player.health = MAX_HEALTH;
    player.dead = false;
    player.moving = false;
    player.attack_target_id = None;
    player.attack_cooldown = 0.0;
    player.respawn_timer = 0.0;
    cancel_attack(player);
}

fn face_position(player: &mut Player, target: Vec2) {
    let dx = target.x - player.position.x;
    let dz = target.z - player.position.z;

    if dx.abs() < 0.001 && dz.abs() < 0.001 {
        return;
    }

    player.facing = dx.atan2(dz);
}

fn distance_between(a: Vec2, b: Vec2) -> f32 {
    let dx = b.x - a.x;
    let dz = b.z - a.z;

    (dx * dx + dz * dz).sqrt()
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
        player.spawn = spawn;
        player.target = spawn;
        player.facing = 0.0;
        player.health = MAX_HEALTH;
        player.dead = false;
        player.moving = false;
        player.attack_target_id = None;
        player.attack_cooldown = 0.0;
        player.respawn_timer = 0.0;
        cancel_attack(player);
    }
}

fn make_player(name: String, mercenary_id: String) -> Player {
    Player {
        id: make_player_id(),
        name: clean_name(name),
        mercenary_id: clean_mercenary_id(mercenary_id),
        position: Vec2 { x: 0.0, z: 0.0 },
        spawn: Vec2 { x: 0.0, z: 0.0 },
        target: Vec2 { x: 0.0, z: 0.0 },
        facing: 0.0,
        health: MAX_HEALTH,
        dead: false,
        moving: false,
        attacking: false,
        attack_target_id: None,
        attack_hit_target_id: None,
        attack_cooldown: 0.0,
        attack_timer: 0.0,
        attack_damage_pending: false,
        respawn_timer: 0.0,
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
            health: player.health,
            dead: player.dead,
            moving: player.moving,
            attacking: player.attacking,
            attack_target_id: player.attack_target_id.clone(),
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
