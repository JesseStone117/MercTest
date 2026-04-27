export type AnimationKind = 'idle' | 'walk';
export type MercenaryId = 'polilock' | 'welstoce';

const animationNames: Record<AnimationKind, string> = {
  idle: 'CharacterArmature|Idle',
  walk: 'CharacterArmature|Walk'
};

export function getAnimationName(_mercenaryId: MercenaryId, kind: AnimationKind): string {
  return animationNames[kind];
}
