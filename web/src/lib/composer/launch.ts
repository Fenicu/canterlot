import { durationMs } from '#lib/motion.js';

/**
 * Отправка с нажатия: стрелка улетает вверх из круглой кнопки, а новая поднимается снизу. Подтверждение
 * стоит на самой кнопке, до ответа хаба. Только прозрачность и сдвиг одного значка; кнопка обрезает его
 * своим кругом (`overflow: clip`). Без движения, системного или масштабом 0, стрелка стоит на месте.
 * Вызывается только с нажатия: отправка клавишей Enter остаётся мгновенной.
 */
export function launch(button: HTMLElement | null): void {
  const arrow = button?.querySelector('svg');
  if (!arrow || !button || !durationMs('--duration-press') || typeof arrow.animate !== 'function') return;
  const rise = `${button.getBoundingClientRect().height / 2}px`;
  const style = getComputedStyle(arrow);
  const leaving = arrow.animate([{ opacity: 1, transform: 'none' }, { opacity: 0, transform: `translateY(-${rise})` }], {
    duration: durationMs('--duration-press'),
    easing: style.getPropertyValue('--ease-press').trim(),
    fill: 'forwards'
  });
  leaving.onfinish = () => {
    arrow.animate([{ opacity: 0, transform: `translateY(${rise})` }, { opacity: 1, transform: 'none' }], { duration: durationMs('--duration-enter'), easing: style.getPropertyValue('--ease-enter').trim() });
    leaving.cancel();
  };
}
