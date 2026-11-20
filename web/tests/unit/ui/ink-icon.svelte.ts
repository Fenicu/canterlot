import type { IconName } from '../../../src/lib/ui/icons';
import type { InkState } from '../../../src/lib/ui/ink';

/** Свойства значка, которые тест меняет на лету: так проверяются переходы состояний. */
export function inkProps(start: InkState, name: IconName = 'brain') {
  let state = $state(start);
  return {
    name,
    get state() {
      return state;
    },
    set state(next: InkState) {
      state = next;
    }
  };
}
