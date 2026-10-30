export const icons = {
  back: 'm13 5-7 7 7 7', close: 'm7 7 10 10M7 17 17 7',
  moon: 'M20 14A9 9 0 0 1 10 3a9 9 0 1 0 10 11Z',
  sun: 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8M12 2v2m0 16v2M2 12h2m16 0h2M5 5l2 2m10 10 2 2M5 19l2-2M17 7l2-2',
  attach: 'm8 12 7-7a4 4 0 0 1 6 6L10 22a6 6 0 0 1-8-8L14 2m-9 15L17 5',
  send: 'M12 20V4m-7 7 7-7 7 7', stop: 'M6 6h12v12H6Z', info: 'M12 11v6m0-10v1M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20',
  search: 'M10 3a7 7 0 1 0 0 14 7 7 0 0 0 0-14m5 12 6 6', more: 'M4 12a1 1 0 1 0 2 0 1 1 0 1 0-2 0m7 0a1 1 0 1 0 2 0 1 1 0 1 0-2 0m7 0a1 1 0 1 0 2 0 1 1 0 1 0-2 0', plus: 'M12 5v14M5 12h14',
  chevron: 'm9 6 6 6-6 6', copy: 'M9 9h12v12H9ZM3 15V3h12', branch: 'M6 3v18m0-9h8a4 4 0 0 0 4-4V3',
  share: 'M12 16V3m-5 5 5-5 5 5M5 13v8h14v-8',
  trash: 'M3 6h18M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7', pin: 'm8 3 8 0-1 7 4 4H5l4-4ZM12 14v8',
  warning: 'M12 3 2 21h20ZM12 9v5m0 3v1',
  down: 'm6 9 6 6 6-6', up: 'm6 15 6-6 6 6', sliders: 'M4 7h9m4 0h3M13 4v6M4 17h3m4 0h9M11 14v6',
  folder: 'M3 6h6l2 3h10v10H3Z', download: 'M12 4v12m-5-5 5 5 5-5M5 20h14', archive: 'M3 5h18v4H3ZM5 9v10h14V9M10 13h4',
  edit: 'M4 20h4L20 8l-4-4L4 16Zm10-14 4 4',
  retry: 'M4 12a8 8 0 1 0 2.5-5.8M4 4v5h5',
  clock: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18Zm0 4v5l3 2'
} as const;
export type IconName = keyof typeof icons;
