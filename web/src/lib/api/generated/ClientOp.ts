export type ClientOp =
  | {
      op: "subscribe";
      id: string;
      scope: Scope;
      after?: Seq;
    }
  | {
      op: "unsubscribe";
      id: string;
    }
  | {
      op: "ping";
      t: number;
    };
export type Scope = string;
export type Seq = string;
