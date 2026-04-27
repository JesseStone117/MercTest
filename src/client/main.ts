import * as THREE from 'three';
import { GLTFLoader, type GLTF } from 'three/examples/jsm/loaders/GLTFLoader.js';
import * as SkeletonUtils from 'three/examples/jsm/utils/SkeletonUtils.js';
import { getAnimationName, type MercenaryId } from './animation';
import type { ClientMessage, PlayerState, ServerMessage } from './protocol';
import './styles.css';

const mapHalfSize = 10;
const modelScale = 1.15;
const serverUrl = `ws://${window.location.hostname || '127.0.0.1'}:4000/ws`;

const modelUrls: Record<MercenaryId, string> = {
  polilock: new URL('../../models/Polilock.glb', import.meta.url).href,
  welstoce: new URL('../../models/Welstoce.glb', import.meta.url).href
};

type RenderPlayer = {
  id: string;
  mercenaryId: MercenaryId;
  group: THREE.Group;
  mixer: THREE.AnimationMixer;
  actions: Map<string, THREE.AnimationAction>;
  currentActionName: string;
  visualPosition: THREE.Vector3;
  serverPosition: THREE.Vector3;
  moving: boolean;
};

type LobbyState = {
  connected: boolean;
  playerId: string;
  code: string;
  hostId: string;
  started: boolean;
  players: PlayerState[];
};

const lobby: LobbyState = {
  connected: false,
  playerId: '',
  code: '',
  hostId: '',
  started: false,
  players: []
};

const canvasElement = document.querySelector<HTMLCanvasElement>('#game');
const lobbyPanel = document.querySelector<HTMLElement>('#lobbyPanel');
const nameInput = document.querySelector<HTMLInputElement>('#nameInput');
const mercenaryInput = document.querySelector<HTMLSelectElement>('#mercenaryInput');
const codeInput = document.querySelector<HTMLInputElement>('#codeInput');
const createButton = document.querySelector<HTMLButtonElement>('#createButton');
const joinButton = document.querySelector<HTMLButtonElement>('#joinButton');
const startButton = document.querySelector<HTMLButtonElement>('#startButton');
const lobbyInfo = document.querySelector<HTMLElement>('#lobbyInfo');
const codeLabel = document.querySelector<HTMLElement>('#codeLabel');
const playerList = document.querySelector<HTMLElement>('#playerList');
const statusText = document.querySelector<HTMLElement>('#statusText');
const hud = document.querySelector<HTMLElement>('#hud');
const hudCode = document.querySelector<HTMLElement>('#hudCode');
const hudPlayers = document.querySelector<HTMLElement>('#hudPlayers');

if (!canvasElement) {
  throw new Error('Missing #game canvas.');
}

const canvas = canvasElement;

const scene = new THREE.Scene();
scene.background = new THREE.Color(0x141611);

const camera = new THREE.PerspectiveCamera(45, window.innerWidth / window.innerHeight, 0.1, 100);
camera.position.set(0, 15, 14);
camera.lookAt(0, 0, 0);

const renderer = new THREE.WebGLRenderer({ canvas, antialias: true });
renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
renderer.setSize(window.innerWidth, window.innerHeight);
renderer.shadowMap.enabled = true;

const clock = new THREE.Clock();
const raycaster = new THREE.Raycaster();
const pointer = new THREE.Vector2();
const groundPlane = new THREE.Plane(new THREE.Vector3(0, 1, 0), 0);
const gltfLoader = new GLTFLoader();
const players = new Map<string, RenderPlayer>();
const loadingPlayers = new Set<string>();
const modelCache = new Map<MercenaryId, GLTF>();

let socket: WebSocket | null = null;
let moveMarker: THREE.Mesh | null = null;

setupWorld();
connect();
bindUi();
animate();

function setupWorld(): void {
  const sun = new THREE.DirectionalLight(0xfff1c8, 2.6);
  sun.position.set(-4, 10, 6);
  sun.castShadow = true;
  scene.add(sun);

  const ambient = new THREE.HemisphereLight(0xdde7ff, 0x29331f, 1.4);
  scene.add(ambient);

  const floor = new THREE.Mesh(
    new THREE.PlaneGeometry(mapHalfSize * 2, mapHalfSize * 2),
    new THREE.MeshStandardMaterial({ color: 0x52613c, roughness: 0.92 })
  );
  floor.rotation.x = -Math.PI / 2;
  floor.receiveShadow = true;
  scene.add(floor);

  const grid = new THREE.GridHelper(mapHalfSize * 2, 20, 0xf4c95d, 0x87946e);
  grid.position.y = 0.02;
  scene.add(grid);

  const edge = new THREE.LineSegments(
    new THREE.EdgesGeometry(new THREE.BoxGeometry(mapHalfSize * 2, 0.18, mapHalfSize * 2)),
    new THREE.LineBasicMaterial({ color: 0xf4c95d })
  );
  edge.position.y = 0.09;
  scene.add(edge);
}

function connect(): void {
  socket = new WebSocket(serverUrl);

  socket.addEventListener('open', () => {
    lobby.connected = true;
    setStatus('Connected. Create a lobby or join with a 4 digit code.');
    renderUi();
  });

  socket.addEventListener('message', (event) => {
    const message = JSON.parse(String(event.data)) as ServerMessage;
    handleServerMessage(message);
  });

  socket.addEventListener('close', () => {
    lobby.connected = false;
    setStatus('Disconnected. Restart the server and refresh the page.');
    renderUi();
  });
}

function bindUi(): void {
  createButton?.addEventListener('click', () => {
    send({
      type: 'create_lobby',
      name: playerName(),
      mercenaryId: selectedMercenary()
    });
  });

  joinButton?.addEventListener('click', () => {
    send({
      type: 'join_lobby',
      code: lobbyCode(),
      name: playerName(),
      mercenaryId: selectedMercenary()
    });
  });

  startButton?.addEventListener('click', () => {
    send({ type: 'start_game' });
  });

  canvas.addEventListener('contextmenu', (event) => event.preventDefault());
  canvas.addEventListener('pointerdown', handlePointerDown);
  window.addEventListener('resize', resize);
}

function handleServerMessage(message: ServerMessage): void {
  if (message.type === 'error') {
    setStatus(message.message);
    return;
  }

  if (message.type === 'lobby_joined') {
    lobby.playerId = message.playerId;
    lobby.code = message.code;
    lobby.hostId = message.hostId;
    lobby.started = message.started;
    lobby.players = message.players;
    setStatus('Lobby ready.');
    renderUi();
    syncPlayers(message.players);
    return;
  }

  if (message.type === 'lobby_update') {
    lobby.code = message.code;
    lobby.hostId = message.hostId;
    lobby.started = message.started;
    lobby.players = message.players;
    renderUi();
    syncPlayers(message.players);
    return;
  }

  if (message.type === 'game_started') {
    lobby.started = true;
    lobby.players = message.players;
    setStatus('');
    renderUi();
    syncPlayers(message.players);
    return;
  }

  lobby.players = message.players;
  renderUi();
  syncPlayers(message.players);
}

function handlePointerDown(event: PointerEvent): void {
  if (event.button !== 2 || !lobby.started) {
    return;
  }

  const point = worldPointFromPointer(event);

  if (!point) {
    return;
  }

  const x = clamp(point.x, -mapHalfSize, mapHalfSize);
  const z = clamp(point.z, -mapHalfSize, mapHalfSize);

  send({ type: 'move_to', x, z });
  showMoveMarker(x, z);
}

function worldPointFromPointer(event: PointerEvent): THREE.Vector3 | null {
  const rect = canvas.getBoundingClientRect();
  pointer.x = ((event.clientX - rect.left) / rect.width) * 2 - 1;
  pointer.y = -(((event.clientY - rect.top) / rect.height) * 2 - 1);
  raycaster.setFromCamera(pointer, camera);

  const point = new THREE.Vector3();
  const didHit = raycaster.ray.intersectPlane(groundPlane, point);

  return didHit ? point : null;
}

function send(message: ClientMessage): void {
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    setStatus('Server is not connected yet.');
    return;
  }

  socket.send(JSON.stringify(message));
}

function renderUi(): void {
  const connected = lobby.connected;
  const inLobby = Boolean(lobby.code);
  const isHost = lobby.playerId === lobby.hostId;

  if (createButton) {
    createButton.disabled = !connected || inLobby;
  }

  if (joinButton) {
    joinButton.disabled = !connected || inLobby;
  }

  if (startButton) {
    startButton.disabled = !isHost || lobby.started || lobby.players.length === 0;
  }

  lobbyInfo?.classList.toggle('hidden', !inLobby || lobby.started);
  lobbyPanel?.classList.toggle('hidden', lobby.started);
  hud?.classList.toggle('hidden', !lobby.started);

  setText(codeLabel, lobby.code);
  setText(hudCode, lobby.code);
  setText(hudPlayers, `${lobby.players.length} player${lobby.players.length === 1 ? '' : 's'}`);
  renderPlayerList();
}

function renderPlayerList(): void {
  if (!playerList) {
    return;
  }

  playerList.replaceChildren(
    ...lobby.players.map((player) => {
      const item = document.createElement('li');
      const name = document.createElement('span');
      const mercenary = document.createElement('strong');

      name.textContent = player.id === lobby.hostId ? `${player.name} (host)` : player.name;
      mercenary.textContent = player.mercenaryId;

      item.append(name, mercenary);
      return item;
    })
  );
}

function syncPlayers(serverPlayers: PlayerState[]): void {
  const serverIds = new Set(serverPlayers.map((player) => player.id));

  for (const player of serverPlayers) {
    updateRenderPlayer(player);
  }

  for (const [id, player] of players) {
    if (serverIds.has(id)) {
      continue;
    }

    scene.remove(player.group);
    players.delete(id);
  }
}

function updateRenderPlayer(player: PlayerState): void {
  const renderPlayer = players.get(player.id);

  if (!renderPlayer) {
    loadRenderPlayer(player);
    return;
  }

  renderPlayer.serverPosition.set(player.x, 0, player.z);
  renderPlayer.group.rotation.y = player.facing;
  renderPlayer.moving = player.moving;
  setPlayerAnimation(renderPlayer);
}

function loadRenderPlayer(player: PlayerState): void {
  if (loadingPlayers.has(player.id)) {
    return;
  }

  loadingPlayers.add(player.id);

  loadModel(player.mercenaryId)
    .then((gltf) => {
      const group = SkeletonUtils.clone(gltf.scene) as THREE.Group;
      const mixer = new THREE.AnimationMixer(group);
      const actions = new Map<string, THREE.AnimationAction>();

      for (const clip of gltf.animations) {
        actions.set(clip.name, mixer.clipAction(clip));
      }

      group.scale.setScalar(modelScale);
      group.position.set(player.x, 0, player.z);
      group.rotation.y = player.facing;
      group.traverse((child: THREE.Object3D) => {
        if (child instanceof THREE.Mesh) {
          child.castShadow = true;
          child.receiveShadow = true;
        }
      });

      const renderPlayer: RenderPlayer = {
        id: player.id,
        mercenaryId: player.mercenaryId,
        group,
        mixer,
        actions,
        currentActionName: '',
        visualPosition: group.position.clone(),
        serverPosition: new THREE.Vector3(player.x, 0, player.z),
        moving: player.moving
      };

      players.set(player.id, renderPlayer);
      scene.add(group);
      setPlayerAnimation(renderPlayer);
    })
    .finally(() => {
      loadingPlayers.delete(player.id);
    });
}

async function loadModel(mercenaryId: MercenaryId): Promise<GLTF> {
  const cached = modelCache.get(mercenaryId);

  if (cached) {
    return cached;
  }

  const gltf = await gltfLoader.loadAsync(modelUrls[mercenaryId]);
  modelCache.set(mercenaryId, gltf);
  return gltf;
}

function setPlayerAnimation(player: RenderPlayer): void {
  const localAnimationState = { mercenaryId: player.mercenaryId };
  const walkAnim = getAnimationName(localAnimationState.mercenaryId, 'walk');
  const idleAnim = getAnimationName(localAnimationState.mercenaryId, 'idle');
  const nextActionName = player.moving ? walkAnim : idleAnim;

  if (player.currentActionName === nextActionName) {
    return;
  }

  const nextAction = player.actions.get(nextActionName);

  if (!nextAction) {
    return;
  }

  const previousAction = player.actions.get(player.currentActionName);
  nextAction.reset().fadeIn(0.12).play();

  if (previousAction) {
    previousAction.fadeOut(0.12);
  }

  player.currentActionName = nextActionName;
}

function animate(): void {
  const delta = clock.getDelta();
  const followAmount = 1 - Math.exp(-18 * delta);

  for (const player of players.values()) {
    player.visualPosition.lerp(player.serverPosition, followAmount);
    player.group.position.copy(player.visualPosition);
    player.mixer.update(delta);
  }

  renderer.render(scene, camera);
  requestAnimationFrame(animate);
}

function showMoveMarker(x: number, z: number): void {
  if (!moveMarker) {
    moveMarker = new THREE.Mesh(
      new THREE.RingGeometry(0.28, 0.34, 32),
      new THREE.MeshBasicMaterial({ color: 0xf4c95d, side: THREE.DoubleSide })
    );
    moveMarker.rotation.x = -Math.PI / 2;
    scene.add(moveMarker);
  }

  moveMarker.position.set(x, 0.04, z);
}

function resize(): void {
  camera.aspect = window.innerWidth / window.innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(window.innerWidth, window.innerHeight);
}

function playerName(): string {
  const name = nameInput?.value.trim() || 'Player';
  return name.slice(0, 16);
}

function selectedMercenary(): MercenaryId {
  return mercenaryInput?.value === 'welstoce' ? 'welstoce' : 'polilock';
}

function lobbyCode(): string {
  return codeInput?.value.trim().slice(0, 4) || '';
}

function setStatus(text: string): void {
  setText(statusText, text);
}

function setText(element: HTMLElement | null, text: string): void {
  if (!element) {
    return;
  }

  element.textContent = text;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}
