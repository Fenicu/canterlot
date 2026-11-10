#!/usr/bin/python3
import json, os, sys, pathlib, signal, select, time, subprocess
d=pathlib.Path(__file__).parent
if '--version' in sys.argv:
    count=d/'version-count'
    try: n=int(count.read_text())
    except Exception: n=0
    count.write_text(str(n+1))
    delay=d/'version-delay'
    if delay.exists():
        wait=float(delay.read_text())/1000
        delay.unlink()
        time.sleep(wait)
    v=(d/'version').read_text() if (d/'version').exists() else '2.1.311'
    print(v); sys.exit(1 if v=='failed' else 0)
(d/'args.json').write_text(json.dumps(sys.argv[1:]))
(d/'connectors.json').write_text(json.dumps(os.environ.get('ENABLE_CLAUDEAI_MCP_SERVERS')))
sid=sys.argv[sys.argv.index('--resume')+1] if '--resume' in sys.argv else sys.argv[sys.argv.index('--session-id')+1]
def emit(v):
    if isinstance(v,str): print(v,flush=True); return
    if v.get('subtype')=='init': v=dict(v,session_id=sid)
    print(json.dumps(v),flush=True)
def interrupted(*_):
    for v in json.loads((d/'interrupt.json').read_text()): emit(v)
    sys.exit(0)
signal.signal(signal.SIGINT,interrupted)
relay=None
for n,line in enumerate(sys.stdin,1):
    with (d/'stdin.log').open('a') as f: f.write(line)
    v=json.loads(line)
    if v.get('type')=='control_request':
        emit({'type':'control_response','response':{'subtype':'success','request_id':v['request_id'],'response':{}}}); continue
    if (d/'init-delay').exists(): time.sleep(int((d/'init-delay').read_text())/1000)
    init_mode=(d/'init-mode').read_text() if (d/'init-mode').exists() else 'before'
    init={'type':'system','subtype':'init','session_id':sid,'model':'claude-haiku-test','permissionMode':'default','mcp_servers':[{'name':'canterlot','status':'connected'}]}
    if init_mode=='before': emit(init)
    frames=json.loads((d/('turn'+str(n)+'.json')).read_text())
    for frame in frames:
        if frame=='@probe':
            if select.select([sys.stdin],[],[],0.2)[0]: (d/'early').write_text('early input')
        elif isinstance(frame,str) and frame.startswith('@request:'):
            if relay is None:
                mcp_config=pathlib.Path(sys.argv[sys.argv.index('--mcp-config')+1])
                exec_dir=mcp_config.parent
                server_env=json.loads(mcp_config.read_text())['mcpServers']['canterlot'].get('env',{})
                relay=subprocess.Popen([(d/'relay-exe').read_text(),'mcp-relay','--socket',str(exec_dir/'mcp.sock')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,env={**os.environ,**server_env})
            call=json.loads((d/'mcp-calls.json').read_text())[int(frame.split(':')[1])]
            relay.stdin.write(json.dumps(call)+'\n'); relay.stdin.flush()
            if not select.select([relay.stdout],[],[],30)[0]: raise RuntimeError('MCP reply timeout')
            with (d/'mcp-replies.jsonl').open('a') as replies: replies.write(relay.stdout.readline())
        elif frame=='@wait':
            (d/'waiting').write_text('1')
            while True: time.sleep(1)
        elif isinstance(frame,str) and frame.startswith('@sleep:'): time.sleep(int(frame.split(':')[1])/1000)
        elif frame=='@exit': sys.exit(7)
        else:
            # "@input" в user_message_uuids и command_uuid заменяется UUID присланного ввода.
            if isinstance(frame,dict):
                if frame.get('user_message_uuids')==['@input']: frame=dict(frame,user_message_uuids=[v['uuid']])
                if frame.get('command_uuid')=='@input': frame=dict(frame,command_uuid=v['uuid'])
            emit(frame)
    if init_mode=='after': emit(init)
if relay is not None:
    relay.stdin.close(); relay.wait(timeout=10)
