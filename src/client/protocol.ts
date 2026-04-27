import type { MercenaryId } from './animation';

export type ClientMessage =
  | {
      type: 'create_lobby';
      name: string;
      mercenaryId: MercenaryId;
    }
  | {
      type: 'join_lobby';
      code: string;
      name: string;
      mercenaryId: MercenaryId;
    }
  | {
      type: 'start_game';
    }
  | {
      type: 'move_to';
      x: number;
      z: number;
    };

export type ServerMessage =
  | {
      type: 'lobby_joined';
      code: string;
      playerId: string;
      hostId: string;
      started: boolean;
      players: PlayerState[];
    }
  | {
      type: 'lobby_update';
      code: string;
      hostId: string;
      started: boolean;
      players: PlayerState[];
    }
  | {
      type: 'game_started';
      players: PlayerState[];
    }
  | {
      type: 'state';
      tick: number;
      players: PlayerState[];
    }
  | {
      type: 'error';
      message: string;
    };

export type PlayerState = {
  id: string;
  name: string;
  mercenaryId: MercenaryId;
  x: number;
  z: number;
  facing: number;
  moving: boolean;
};
