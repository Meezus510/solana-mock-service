"""Local mock protocol adapter; rejects host namespace and non-loopback URLs."""
import json,os,sys,urllib.request,urllib.parse
assert os.environ.get('SNAPSHOT_MOCK_ISOLATED')=='1'
assert os.readlink('/proc/self/ns/net')!=os.readlink('/proc/1/ns/net')
origin=os.environ['MOCK_TEST_ORIGIN']
assert urllib.parse.urlsplit(origin).hostname=='127.0.0.1'
for line in sys.stdin:
    request=json.loads(line);payload=request['payload']
    if payload['op']=='tracked_mints':
        result={'status':'OK','mints':['So11111111111111111111111111111111111111112']}
    else:
        req=urllib.request.Request(origin+'/telegram/provider',data=json.dumps(payload).encode(),headers={'Content-Type':'application/json'})
        with urllib.request.urlopen(req,timeout=5) as response:result=json.load(response)
    print(json.dumps({'protocol':request['protocol'],'request_id':request['request_id'],'payload':result}),flush=True)
