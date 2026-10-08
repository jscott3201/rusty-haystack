"""Mutation-tested release authority and producer/consumer wiring contracts.

The repository's canonical two-space YAML layout is intentional here. This is
not a YAML parser; actionlint separately validates syntax and expressions.
"""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[3]
GUARD = "    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"
BINARY_KEYS = ('cli_linux_x86_64','cli_linux_aarch64','cli_macos_x86_64','cli_macos_aarch64','cli_windows_x86_64')
WHEEL_KEYS = tuple('wheel_'+platform+'_'+py for platform in ('linux_x86_64','linux_aarch64','macos_x86_64','macos_aarch64') for py in ('cp311','cp312','cp313'))


def jobs(workflow):
    body=workflow.split('\njobs:\n',1)[1]
    headers=list(re.finditer(r'^  ([a-z][a-z0-9-]*):[ \t]*$',body,re.MULTILINE))
    return {match[1]:body[match.end():headers[index+1].start() if index+1<len(headers) else len(body)] for index,match in enumerate(headers)}


def steps(job):
    starts=list(re.finditer(r'^      - ',job,re.MULTILINE))
    return [job[match.start():starts[index+1].start() if index+1<len(starts) else len(job)] for index,match in enumerate(starts)]


class WorkflowContracts(unittest.TestCase):
    def setUp(self):
        self.workflows={name:(ROOT/'.github/workflows'/name).read_text() for name in ('release.yml','python.yml','artifact-qualification.yml')}

    def needs(self,job):
        match=re.search(r'^    needs: (.+)$',job,re.MULTILINE)
        self.assertIsNotNone(match)
        return {part.strip() for part in match[1].strip('[]').split(',')}

    def check(self, workflows):
        release,python,native=(jobs(workflows[name]) for name in ('release.yml','python.yml','artifact-qualification.yml'))
        for text in workflows.values():
            self.assertIn('\npermissions:\n  contents: read\n',text)
            self.assertNotIn('overwrite: true',text)
        for job,prerequisite in ((release['publish-crates'],'qualify-binaries'),(release['github-release'],'qualify-binaries'),(python['publish-pypi'],'qualify-python')):
            self.assertIn('\n'+GUARD+'\n',job)
            self.assertIn(prerequisite,self.needs(job))
        self.assertIn('Crates are separately packaged from source',release['publish-crates'])
        for job,action,path in ((release['github-release'],'softprops/action-gh-release@v2','files: ${{ runner.temp }}/publication/*'),(python['publish-pypi'],'pypa/gh-action-pypi-publish@release/v1','packages-dir: ${{ runner.temp }}/publication')):
            self.assertIn('artifacts.py check-staged',job)
            self.assertLess(job.index('artifacts.py check-staged'),job.index(action))
            self.assertIn(path,job)
            self.assertIn('--receipt "$RUNNER_TEMP/proof/stage.json"',job)
        for job,keys in ((release['build-binaries'],BINARY_KEYS),(python['build-wheels'],WHEEL_KEYS),(native['build'],('cli','wheel','sdist'))):
            outputs=re.search(r'\n    outputs:\n(.*?)\n    (?!  )',job,re.DOTALL)
            self.assertIsNotNone(outputs)
            pairs=re.findall(r'^      ([a-z0-9_]+): \$\{\{ steps.identity.outputs.([a-z0-9_]+) \}\}$',outputs[1],re.MULTILINE)
            self.assertEqual({key for key,_ in pairs},set(keys))
            self.assertEqual(len(pairs),len(keys))
            self.assertTrue(all(key==value for key,value in pairs))
            self.assertIn('id: identity',job)
            self.assertIn('ARTIFACT_ID: ${{ steps.candidate.outputs.artifact-id }}',job)
            self.assertIn('echo "$OUTPUT_KEY=$ARTIFACT_ID" >> "$GITHUB_OUTPUT"',job)
            self.assertIn('${{ github.run_attempt }}',job)
        for job in (release['qualify-binaries'],python['qualify-python'],native['qualify'],release['github-release'],python['publish-pypi']):
            self.assertIn('PRODUCERS_JSON: ${{ toJSON(needs) }}',job)
            self.assertIn('artifacts.py download-ids',job)
            self.assertIn('--github-output "$GITHUB_OUTPUT"',job)
            downloads=[step for step in steps(job) if 'uses: actions/download-artifact@v4' in step]
            self.assertTrue(downloads)
            for download in downloads:
                self.assertRegex(download,r'artifact-ids: \$\{\{ steps.inventory.outputs.(artifact-ids|packages|proof) \}\}')
                self.assertNotRegex(download,r'(?m)^          (name|pattern|run-id|repository|github-token):')
                self.assertNotIn('github.run_attempt',download)
        for job in (release['qualify-binaries'],python['qualify-python']):
            self.assertIn('packages: ${{ steps.qualified.outputs.artifact-id }}',job)
            self.assertIn('proof: ${{ steps.proof.outputs.artifact-id }}',job)
            self.assertIn('id: qualified',job)
            self.assertIn('id: proof',job)
            self.assertIn('path: ${{ runner.temp }}/publication',job)
        manual=workflows['artifact-qualification.yml']
        self.assertEqual(set(native),{'build','qualify'})
        self.assertIn('\non:\n  workflow_dispatch:\n',manual)
        self.assertNotRegex(manual,r'(?m)^  (push|pull_request|workflow_call):')
        self.assertNotRegex(manual,r'(secrets\.|id-token:|contents: write|gh-action-pypi-publish|action-gh-release|cargo publish)')

    def test_crate_publication_order_includes_application_dependency(self):
        publication=steps(jobs(self.workflows['release.yml'])['publish-crates'])
        published=[]
        for index,step in enumerate(publication):
            match=re.search(r'run: cargo publish -p ([a-z-]+)',step)
            if match:
                published.append(match[1])
                if match[1] != 'rusty-haystack-cli':
                    self.assertIn('run: sleep 30',publication[index+1])
        self.assertEqual(published,[
            'rusty-haystack-core','rusty-haystack-client','rusty-haystack-app',
            'rusty-haystack-server','rusty-haystack-cli',
        ])
        self.assertIn('COPY haystack-app/ haystack-app/',(ROOT/'Dockerfile').read_text())

    def test_live_authority_and_producer_identity_contract(self):
        self.check(self.workflows)

    def test_each_publication_guard_and_qualification_edge_is_protected(self):
        for file,job_name in (('release.yml','publish-crates'),('release.yml','github-release'),('python.yml','publish-pypi')):
            original=self.workflows[file]; job=jobs(original)[job_name]
            for changed_job in (job.replace(GUARD,"    if: always()"), re.sub(r'(?m)^    needs: .+$','    needs: validate',job)):
                with self.subTest(file=file,job=job_name):
                    changed=self.workflows | {file:original.replace(job,changed_job)}
                    with self.assertRaises(AssertionError): self.check(changed)

    def test_transfer_and_publication_mutations_are_detected(self):
        mutations=[
            ('release.yml','artifacts.py check-staged','artifacts.py verify'),
            ('python.yml','packages-dir: ${{ runner.temp }}/publication','packages-dir: dist'),
            ('release.yml','files: ${{ runner.temp }}/publication/*','files: ${{ runner.temp }}/*'),
            ('python.yml','PRODUCERS_JSON: ${{ toJSON(needs) }}','PRODUCERS_JSON: "{}"'),
            ('artifact-qualification.yml','artifact-ids: ${{ steps.inventory.outputs.artifact-ids }}','name: native-candidate-cli-${{ github.run_attempt }}'),
            ('artifact-qualification.yml','  contents: read','  contents: write'),
            ('release.yml','artifact-ids: ${{ steps.inventory.outputs.artifact-ids }}','artifact-ids: ${{ steps.inventory.outputs.artifact-ids }}\n          run-id: 123'),
            ('python.yml','proof: ${{ steps.proof.outputs.artifact-id }}','proof: ${{ steps.qualified.outputs.artifact-id }}'),
            ('python.yml','wheel_linux_x86_64_cp311: ${{ steps.identity.outputs.wheel_linux_x86_64_cp311 }}','wheel_linux_x86_64_cp312: ${{ steps.identity.outputs.wheel_linux_x86_64_cp311 }}'),
        ]
        for file,before,after in mutations:
            with self.subTest(file=file,mutation=before):
                self.assertIn(before,self.workflows[file])
                changed=self.workflows | {file:self.workflows[file].replace(before,after)}
                with self.assertRaises(AssertionError): self.check(changed)


if __name__=='__main__':
    unittest.main()
