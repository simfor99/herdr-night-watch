"""Transport-only tests: no real watcher, agent, warning, or power command."""
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest

DIRECTORY = Path(__file__).resolve().parent
BOOT_TICKS = '134362026865000000'


class StatusChannelTests(unittest.TestCase):
    def start(self, body, setup=''):
        script = f'''import sys,types,os,time,signal
sys.path.insert(0,{str(DIRECTORY)!r})
import status_channel as channel
{setup}
def payload():
{body}
watcher=types.SimpleNamespace(_STATUS_DEADLINE=None,status_payload=payload)
raise SystemExit(channel.serve(watcher))
'''
        child = subprocess.Popen([sys.executable, '-u', '-c', script], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 env={**os.environ, 'HERDR_STATUS_WINDOWS_BOOT_TICKS':BOOT_TICKS})
        self.addCleanup(self.cleanup, child)
        return child

    def cleanup(self, child):
        if child.poll() is None:
            child.stdin.close()
            try:
                child.wait(timeout=4)
            except subprocess.TimeoutExpired:
                child.kill(); child.wait(timeout=2)
        if not child.stdin.closed:
            try:child.stdin.close()
            except BrokenPipeError:pass
        child.stdout.close(); child.stderr.close()

    def request(self, child, request_id=1):
        child.stdin.write((json.dumps({'version':1,'op':'status','id':request_id})+'\n').encode())
        child.stdin.flush()

    def response(self, child, timeout=4):
        self.assertTrue(select.select([child.stdout],[],[],timeout)[0], 'response deadline')
        return json.loads(child.stdout.readline())

    def test_reuse_and_log_isolation_and_boot_marker(self):
        child=self.start("    print('maintenance log')\n    return {'pid':os.getpid(),'boot':watcher._STATUS_WINDOWS_BOOT_ID}")
        self.request(child); first=self.response(child)
        self.request(child,2); second=self.response(child)
        self.assertEqual(first['status']['pid'],second['status']['pid'])
        self.assertEqual(second['sequence'],2)
        self.assertEqual(second['status']['boot'],'2026-10-11T14:31:26.5000000Z')
        self.assertIn('generated_unix_ms',second)
        child.stdin.close(); self.assertTrue(self.response(child)['closed']);child.wait(timeout=3)
        self.assertEqual(child.stderr.read().count(b'maintenance log'),2)

    def test_bad_protocol_is_error_not_off(self):
        child=self.start("    return {}")
        child.stdin.write(b'{"version":9,"id":1,"op":"status"}\n');child.stdin.flush()
        result=self.response(child);self.assertIn('error',result);self.assertNotIn('status',result)

    def test_request_alarm_bounds_hang(self):
        child=self.start("    time.sleep(30)\n    return {}",'channel.REQUEST_SECONDS=0.1')
        self.request(child);result=self.response(child,2)
        self.assertIn('TimeoutError',result['error'])
        child.stdin.close();self.assertTrue(self.response(child)['closed'])

    def test_child_signals_are_unblocked(self):
        body="    result=channel.run_bounded([sys.executable,'-c','import signal;print(sorted(signal.pthread_sigmask(signal.SIG_BLOCK,[])))'],watcher._STATUS_DEADLINE)\n    return {'mask':result.stdout.strip()}"
        child=self.start(body);self.request(child)
        self.assertEqual(self.response(child)['status']['mask'],'[]')

    def test_eof_cleans_command_and_grandchild(self):
        with tempfile.TemporaryDirectory() as folder:
            pidfile=Path(folder)/'pids'
            code=f"import os,subprocess,sys,time;child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']);open({str(pidfile)!r},'w').write(str(os.getpid())+' '+str(child.pid));time.sleep(60)"
            child=self.start(f"    channel.run_bounded([sys.executable,'-c',{code!r}],watcher._STATUS_DEADLINE)\n    return {{}}")
            self.request(child)
            deadline=time.monotonic()+3
            while not pidfile.exists() and time.monotonic()<deadline:time.sleep(.01)
            self.assertTrue(pidfile.exists());pids=[int(x) for x in pidfile.read_text().split()]
            child.stdin.close();self.assertTrue(self.response(child)['closed']);child.wait(timeout=3)
            for pid in pids:
                stat=Path(f'/proc/{pid}/stat')
                self.assertTrue(not stat.exists() or stat.read_text().split()[2]=='Z',f'live descendant {pid}')

    def test_oversized_request_closes_channel(self):
        child=self.start("    return {}")
        try:
            child.stdin.write(b'x'*(256*1024+1)+b'\n');child.stdin.flush()
        except BrokenPipeError:pass
        self.assertTrue(self.response(child)['closed']);child.wait(timeout=3)

class WatcherStatusCompatibilityTests(unittest.TestCase):
    def test_existing_cli_and_payload_match_across_states(self):
        import importlib.util
        import io
        from unittest.mock import patch
        from contextlib import redirect_stdout
        spec=importlib.util.spec_from_file_location('status_fixture_watcher',DIRECTORY/'herdr-night-watch.py')
        watcher=importlib.util.module_from_spec(spec);spec.loader.exec_module(watcher)
        cases=[(None,None),({'dry_run':True},None),
               ({'run_id':'fixture'}, {'warning_id':'fixture-warning','cancelable_until':'2099-01-01T00:00:00+00:00'}),
               ({'outcome':'cancelled'},None)]
        for state,warning in cases:
            with self.subTest(state=state), patch.object(watcher,'reset_stale_run_after_restart'), \
                 patch.object(watcher,'load_json',side_effect=[state,warning,state,warning]), \
                 patch.object(watcher,'utc_now',return_value=watcher.datetime(2026,1,1,tzinfo=watcher.timezone.utc)), \
                 patch.object(watcher,'preferred_completion_action',return_value='sleep'), \
                 patch.object(watcher,'preferred_warning_seconds',return_value=300), \
                 patch.object(watcher,'live_agent_summary',return_value={'available':False}):
                payload=watcher.status_payload()
                output=io.StringIO()
                with redirect_stdout(output): self.assertEqual(watcher.status(),0)
                self.assertEqual(payload,json.loads(output.getvalue()))

    def test_native_boot_marker_skips_powershell_only_in_status_budget(self):
        import importlib.util
        from unittest.mock import patch
        spec=importlib.util.spec_from_file_location('boot_fixture_watcher',DIRECTORY/'herdr-night-watch.py')
        watcher=importlib.util.module_from_spec(spec);spec.loader.exec_module(watcher)
        watcher._STATUS_WINDOWS_BOOT_ID='fixture-native-marker'
        watcher._STATUS_DEADLINE=time.monotonic()+5
        with patch.object(watcher,'powershell_executable',side_effect=AssertionError('console probe')):
            self.assertEqual(watcher.windows_boot_id(),'fixture-native-marker')
        watcher._STATUS_DEADLINE=None
        with patch.object(watcher,'powershell_executable',return_value=None) as probe, \
             patch.object(watcher,'record_diagnostic'):
            self.assertIsNone(watcher.windows_boot_id());probe.assert_called_once()

if __name__=='__main__':unittest.main()
