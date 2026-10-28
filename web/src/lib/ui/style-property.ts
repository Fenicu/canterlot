export function styleProperty(node: HTMLElement | SVGElement, initial: { name: string; value: string | undefined }) {
  function update({ name, value }: typeof initial) {
    if (value === undefined) node.style.removeProperty(name);
    else node.style.setProperty(name, value);
  }
  update(initial);
  return { update };
}
