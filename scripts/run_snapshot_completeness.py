#!/usr/bin/env python3
"""Run only snapshot mocks inside a network namespace and a capped transient cgroup.

Requires root, systemd, PostgreSQL 16, existing pinned toolchains/dependency caches,
and a Python interpreter containing pytest/psycopg (pass --python). No production
configuration is read. All Rust builds are locked/offline and single-job.
"""
import argparse,json,os,re,shutil,subprocess,sys,time,uuid
from datetime import datetime,timezone
from pathlib import Path

REPO=Path(__file__).resolve().parents[1]
DEFAULTS={'mock':REPO,'market':REPO.parent/'market-evidence-service','social':REPO.parent/'social-evidence-service','strategy':REPO.parent/'strategy_service'}

def outer(args):
    assert os.geteuid()==0,'root is required for network isolation'
    run_id=datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    out=(args.output or REPO.parent/f'snapshot-completeness-mocks-{run_id}').resolve();out.mkdir(mode=0o700,parents=True,exist_ok=False)
    command=['systemd-run','--quiet','--wait','--pipe','--collect','--unit',f'snapshot-mock-{run_id.lower()}','-p','CPUQuota=40%','-p','MemoryHigh=1200M','-p','MemoryMax=1800M','-p','TasksMax=128','-p','IOWeight=10','/usr/bin/unshare','--net',args.python,str(Path(__file__).resolve()),'--inside','--output',str(out),'--python',args.python]
    if args.only=='market':
        command += ['--only','market']
    if args.only=='strategy':
        assert args.market_events and args.market_events.is_file(),'--market-events fixture export required'
        command += ['--only','strategy','--market-events',str(args.market_events.resolve())]
    print('Isolated snapshot run:',out,flush=True)
    result=subprocess.run(command)
    print('Artifacts:',out,flush=True)
    return result.returncode


def inside(args):
    assert os.geteuid()==0
    assert os.readlink('/proc/self/ns/net')!=os.readlink('/proc/1/ns/net'),'refusing host network namespace'
    subprocess.run(['ip','link','set','lo','up'],check=True)
    env={'PATH':'/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin','HOME':'/root','CARGO_HOME':'/root/.cargo','RUSTUP_HOME':'/root/.rustup','LANG':'C.UTF-8','CARGO_BUILD_JOBS':'1','RAYON_NUM_THREADS':'1','OMP_NUM_THREADS':'1','OPENBLAS_NUM_THREADS':'1','SNAPSHOT_MOCK_ISOLATED':'1','PAPER_ONLY':'true','REAL_EXECUTION_ENABLED':'false'}
    os.environ.clear();os.environ.update(env)
    out=args.output;pg=Path('/usr/lib/postgresql/16/bin');cluster=Path('/tmp')/f'snapshot-mock-pg-{uuid.uuid4().hex[:12]}'
    cluster.mkdir(mode=0o700);shutil.chown(cluster,user='postgres',group='postgres')
    summary={'started_at':datetime.now(timezone.utc).isoformat(),'network_namespace':os.readlink('/proc/self/ns/net'),'resource_limits':{'cpu_quota':'40% of one CPU','memory_high_mb':1200,'memory_max_mb':1800,'cargo_jobs':1},'commands':[],'repositories':{}}
    processes=[];failed=[]
    def run(name,command,cwd=None,extra=None,timeout=1200):
        print('Running',name,flush=True);began=time.monotonic()
        with (out/(name+'.log')).open('w') as log:
            p=subprocess.run(command,cwd=cwd,env={**env,**(extra or {})},stdout=log,stderr=subprocess.STDOUT,timeout=timeout)
        record={'name':name,'returncode':p.returncode,'duration_seconds':round(time.monotonic()-began,3)};summary['commands'].append(record)
        print(name,'PASS' if p.returncode==0 else 'FAIL',record['duration_seconds'],'seconds',flush=True)
        if p.returncode:failed.append(name)
        return p.returncode==0
    def db(name):return f'host=127.0.0.1 port=55457 user=postgres dbname={name} sslmode=disable options=\'-c statement_timeout=10000 -c lock_timeout=1000\''
    def cargo(*parts):return ['nice','-n','15','ionice','-c','3','cargo',*parts,'--locked','--offline','-j','1']
    def strategy_tests():
        python_env={'PYTHONPATH':str(DEFAULTS['strategy']/'src'),'PYTEST_DISABLE_PLUGIN_AUTOLOAD':'1','STRATEGY_TEST_DSN':db('strategy_simulation'),'MARKET_EVENTS_OUT':str(out/'market-events.json'),'STRATEGY_MATRIX_OUT':str(out/'strategy-matrix.json')}
        run('strategy-snapshot-contracts',[args.python,'-c',"import strategy_service; from pathlib import Path; assert Path(strategy_service.__file__).resolve() == Path('/root/strategy_service/src/strategy_service/__init__.py'); import pytest,sys; sys.exit(pytest.main(sys.argv[1:]))",'-q','--import-mode=importlib',str(DEFAULTS['strategy']/'tests/test_snapshot_completeness_mock.py'),str(DEFAULTS['strategy']/'tests/test_decision_collection_window.py')],Path('/tmp'),python_env)
    try:
        for name,path in DEFAULTS.items():
            summary['repositories'][name]={'path':str(path),'commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=path,text=True).strip(),'dirty_files':subprocess.check_output(['git','status','--short'],cwd=path,text=True).splitlines()}
        assert run('initdb',['runuser','-u','postgres','--',str(pg/'initdb'),'-D',str(cluster/'data'),'-A','trust','--no-locale','--encoding=UTF8'])
        assert run('postgres-start',['runuser','-u','postgres','--',str(pg/'pg_ctl'),'-D',str(cluster/'data'),'-l',str(cluster/'server.log'),'-o',f'-p 55457 -h 127.0.0.1 -k {cluster} -c shared_buffers=32MB -c max_connections=32 -c fsync=off','start'])
        for name in (['strategy_simulation'] if args.only=='strategy' else ['market_simulation','social_simulation','strategy_simulation','recovery_simulation','context_simulation','fence_simulation']):
            assert run('createdb-'+name,['runuser','-u','postgres','--',str(pg/'createdb'),'-h','127.0.0.1','-p','55457',name])
        if args.only=='strategy':
            shutil.copyfile(args.market_events,out/'market-events.json')
            strategy_tests()
            matrix=json.loads((out/'strategy-matrix.json').read_text())
            gaps=[{'owner':'strategy',**g} for g in matrix.get('architecture_discrepancies',[])]
            summary.update(matrix_case_counts={'strategy':len(matrix['cases'])},architecture_discrepancies=gaps,all_production_gaps_confirmed_provider_no_data=False,verdict='INTERNAL_GAPS_DETECTED' if gaps else 'COVERED_FIXTURES_PASS_PRODUCTION_ATTRIBUTION_UNPROVEN')
            return 1 if failed or gaps else 0
        assert run('mock-tests',cargo('test','snapshot_'),DEFAULTS['mock'])
        assert run('mock-build',cargo('build'),DEFAULTS['mock'])
        mock_env={**env,'APP_ENV':'local','MOCK_BIND_ADDR':'127.0.0.1:18080','MOCK_PROVIDERS':'snapshots'}
        mock_log=(out/'mock-runtime.log').open('w')
        mock=subprocess.Popen([str(DEFAULTS['mock']/'target/debug/provider-mock-service')],cwd=DEFAULTS['mock'],env=mock_env,stdout=mock_log,stderr=subprocess.STDOUT);processes.append(mock)
        import urllib.request
        for _ in range(50):
            try:
                with urllib.request.urlopen('http://127.0.0.1:18080/health',timeout=1) as r:assert json.load(r)['status']=='ok'
                break
            except Exception:time.sleep(.1)
        else:raise RuntimeError('local mock did not become ready')
        env.update(MOCK_TEST_ORIGIN='http://127.0.0.1:18080',MOCK_REPO=str(REPO))
        assert run('market-schema',cargo('test','-p','market-evidence-persistence','--test','fresh_initialization'),DEFAULTS['market'],{'FRESH_POSTGRES_TEST_DSN':db('market_simulation')})
        local={'POSTGRES_TEST_DSN':db('market_simulation'),'SNAPSHOT_MATRIX_OUT':str(out/'market-matrix.json'),'MARKET_EVENTS_OUT':str(out/'market-events.json'),'REGIME_MATRIX_OUT':str(out/'regime-matrix.json')}
        run('market-snapshot-matrix',cargo('test','-p','market-evidence-service','--lib','snapshot_mock_matrix_reconciles'),DEFAULTS['market'],local)
        run('provider-error-matrix',cargo('test','-p','market-evidence-provider','--test','simulation_mock_faults'),DEFAULTS['market'],local)
        run('regime-snapshot-matrix',cargo('test','-p','market-evidence-service','--bin','market-regime-collector','snapshot_mock_regime'),DEFAULTS['market'],local)
        run('provider-ohlcv-contracts',cargo('test','-p','market-evidence-provider','--lib','ohlcv'),DEFAULTS['market'])
        run('provider-regime-horizon-contract',cargo('test','-p','market-evidence-provider','--lib','meme_five_minute'),DEFAULTS['market'])
        if args.only=='market':
            matrices={name:json.loads((out/(name+'-matrix.json')).read_text()) for name in ['market','regime']}
            gaps=[{'owner':name,**gap} for name,data in matrices.items() for gap in data.get('architecture_discrepancies',[])]
            summary.update(matrix_case_counts={name:len(data['cases']) for name,data in matrices.items()},architecture_discrepancies=gaps,all_production_gaps_confirmed_provider_no_data=False,verdict='INTERNAL_GAPS_DETECTED' if gaps else 'COVERED_FIXTURES_PASS_PRODUCTION_ATTRIBUTION_UNPROVEN')
            return 1 if failed or gaps else 0
        # Specific existing regressions only; initialize dedicated databases separately.
        for name in ['context_simulation','fence_simulation']:
            run('schema-'+name,cargo('test','-p','market-evidence-persistence','--test','fresh_initialization'),DEFAULTS['market'],{'FRESH_POSTGRES_TEST_DSN':db(name)})
        run('claim-restart-recovery',cargo('test','-p','market-evidence-service','--test','activation_context'),DEFAULTS['market'],{'POSTGRES_TEST_DSN':db('context_simulation'),'RECOVERY_TEST_DSN':db('recovery_simulation')})
        run('absolute-whole-job-deadlines',cargo('test','-p','market-evidence-service','--lib','whole_job_deadline_drops_work_at_signal_and_enrichment_bounds'),DEFAULTS['market'])
        run('lease-publication-fences',cargo('test','-p','market-evidence-service','--lib','postgres_all_publication_paths_reject_expired_and_reassigned_claims'),DEFAULTS['market'],{'POSTGRES_TEST_DSN':db('fence_simulation')})
        import psycopg
        with psycopg.connect(db('market_simulation')) as conn:
            conn.execute("INSERT INTO market_state.provider_authority_generations(authority_generation,authority_owner,provider_mode,activated_at,activation_boundary,live_provider_enabled) VALUES(1,'RUST_MARKET_EVIDENCE','LIVE',now()-interval '1 hour',now()-interval '1 hour',true)")
        run('fast-cancellation-recovery',cargo('test','-p','market-evidence-service','--test','simulation_fast_terminals'),DEFAULTS['market'],local)
        assert run('social-build',cargo('build','--bin','social-evidence-service'),DEFAULTS['social'])
        social_env={'SOCIAL_TEST_DSN':db('social_simulation'),'SOCIAL_MATRIX_OUT':str(out/'social-matrix.json')}
        run('social-page-atomicity',cargo('test','--test','page_batch_postgres'),DEFAULTS['social'],social_env)
        run('social-snapshot-matrix',[args.python,str(DEFAULTS['social']/'tests/snapshot_completeness_mock.py')],DEFAULTS['social'],social_env)
        strategy_tests()
        run('attribution-negative-controls',[args.python,'-m','pytest','-q','--import-mode=importlib',str(REPO/'tests/test_snapshot_attribution.py')],REPO,{'PYTEST_DISABLE_PLUGIN_AUTOLOAD':'1'})
        matrices={name:json.loads((out/(name+'-matrix.json')).read_text()) for name in ['market','regime','social','strategy'] if (out/(name+'-matrix.json')).exists()}
        gaps=[{'owner':name,**gap} for name,data in matrices.items() for gap in data.get('architecture_discrepancies',[])]
        summary.update(matrix_case_counts={name:len(data['cases']) for name,data in matrices.items()},architecture_discrepancies=gaps,execution_failures=failed,all_production_gaps_confirmed_provider_no_data=False,verdict='INTERNAL_GAPS_DETECTED' if gaps else 'COVERED_FIXTURES_PASS_PRODUCTION_ATTRIBUTION_UNPROVEN')
        print('Verdict:',summary['verdict'],'architecture discrepancies:',len(gaps),flush=True)
    except Exception as error:
        summary['runner_error']=str(error);failed.append('runner');print('Runner failure:',error,flush=True)
    finally:
        for p in processes:
            p.terminate()
            try:p.wait(timeout=5)
            except subprocess.TimeoutExpired:p.kill();p.wait()
        subprocess.run(['runuser','-u','postgres','--',str(pg/'pg_ctl'),'-D',str(cluster/'data'),'stop','-m','immediate'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        summary['finished_at']=datetime.now(timezone.utc).isoformat();summary['execution_failures']=failed
        summary['cleanup']='local mock stopped; isolated PostgreSQL stopped; artifacts retained'
        (out/'summary.json').write_text(json.dumps(summary,indent=2))
    return 1 if failed or summary.get('architecture_discrepancies') else 0

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--inside',action='store_true',help=argparse.SUPPRESS);parser.add_argument('--output',type=Path);parser.add_argument('--python',default=sys.executable);parser.add_argument('--only',choices=['all','market','strategy'],default='all');parser.add_argument('--market-events',type=Path)
    args=parser.parse_args();raise SystemExit(inside(args) if args.inside else outer(args))
