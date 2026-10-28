import { Ajv2020 } from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';
import schema from '../../../docs/api/events.schema.json' with { type: 'json' };

const ajv = new Ajv2020({ strict: false, allErrors: false });
addFormats(ajv);
ajv.addSchema(schema);
const event = ajv.getSchema(schema.$id)!;
const server = ajv.compile({ $ref: `${schema.$id}#/$defs/ServerOp` });
const client = ajv.compile({ $ref: `${schema.$id}#/$defs/ClientOp` });
export const clientKinds = new Set(schema.oneOf.map(ref => ref.$ref.split('/').at(-1)!));
export function checkEvent(value: unknown): void { if (!event(value)) throw new Error('fake_hub_invalid_event'); }
export function checkServer(value: unknown): void { if (!server(value)) throw new Error('fake_hub_invalid_server_frame'); }
export function checkClient(value: unknown): boolean { return !!client(value); }
