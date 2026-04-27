export type AnimationKind = 'idle' | 'walk' | 'attack' | 'death';
export type MercenaryId = 'polilock' | 'welstoce';

const animationNames: Record<AnimationKind, string> = {
  idle: 'CharacterArmature|Idle',
  walk: 'CharacterArmature|Walk',
  attack: 'CharacterArmature|Sword_Slash',
  death: 'CharacterArmature|Death'
};

export function getAnimationName(_mercenaryId: MercenaryId, kind: AnimationKind): string {
  return animationNames[kind];
}
