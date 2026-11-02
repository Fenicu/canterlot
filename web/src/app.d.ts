declare global {
  const __APP_VERSION__: string;
  namespace App {
    interface PageState { sheet?: string }
  }
}
export {};
