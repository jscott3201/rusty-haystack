"""Producer identity is independent of the retrying consumer's attempt number."""
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

REPO = Path(__file__).resolve().parents[3]
HELPER = REPO / "scripts/release/artifacts.py"
BINARY_KEYS = ('cli_linux_x86_64', 'cli_linux_aarch64', 'cli_macos_x86_64', 'cli_macos_aarch64', 'cli_windows_x86_64')
WHEEL_KEYS = tuple('wheel_'+platform+'_'+py for platform in ('linux_x86_64','linux_aarch64','macos_x86_64','macos_aarch64') for py in ('cp311','cp312','cp313'))


class DownloadIdentityTests(unittest.TestCase):
    def invoke(self, profile, needs, success=True, attempt='2'):
        result=subprocess.run([sys.executable,str(HELPER),'download-ids','--profile',profile,'--needs-json',json.dumps(needs)],env=os.environ | {'GITHUB_RUN_ATTEMPT':attempt},capture_output=True,text=True)
        self.assertEqual(result.returncode==0,success,result.stdout+result.stderr)
        return json.loads(result.stdout if success else result.stderr)

    def test_successful_producer_ids_survive_a_downstream_attempt_change(self):
        needs={'build':{'result':'success','outputs':{'cli':'101','wheel':'102','sdist':'103'}}}
        first=self.invoke('native',needs,attempt='1')
        retry=self.invoke('native',needs,attempt='2')
        self.assertEqual(first,retry)
        self.assertEqual(retry['artifact-ids'],'101,102,103')

    def test_partial_matrix_output_or_failed_producer_never_falls_back(self):
        complete={key:str(index+101) for index,key in enumerate(BINARY_KEYS)}
        for missing in BINARY_KEYS:
            for value in ('', None):
                outputs=complete.copy()
                if value is None: outputs.pop(missing)
                else: outputs[missing]=value
                self.invoke('binaries',{'build-binaries':{'result':'success','outputs':outputs}},success=False)
        self.invoke('binaries',{'build-binaries':{'result':'failure','outputs':complete}},success=False)
        duplicate=complete.copy(); duplicate[BINARY_KEYS[1]]=duplicate[BINARY_KEYS[0]]
        self.invoke('binaries',{'build-binaries':{'result':'success','outputs':duplicate}},success=False)

    def test_python_and_publication_ids_bind_the_required_successful_jobs(self):
        wheels={key:str(index+201) for index,key in enumerate(WHEEL_KEYS)}
        result=self.invoke('python',{'build-wheels':{'result':'success','outputs':wheels},'build-sdist':{'result':'success','outputs':{'sdist':'300'}}})
        self.assertEqual(len(result['artifact-ids'].split(',')),13)
        needs={'validate':{'result':'success','outputs':{}},'qualify-binaries':{'result':'success','outputs':{'packages':'401','proof':'402'}}}
        result=self.invoke('publish-binaries',needs)
        self.assertEqual((result['packages'],result['proof']),('401','402'))
        needs['qualify-binaries']['outputs']['proof']='not-an-id'
        self.invoke('publish-binaries',needs,success=False)


if __name__=='__main__':
    unittest.main()
