export function ticking(start: number): { value: number } {
  let value = $state(start);
  return {
    get value() {
      return value;
    },
    set value(next: number) {
      value = next;
    },
  };
}
