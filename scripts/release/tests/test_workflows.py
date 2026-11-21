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
WHEEL_KEYS = tuple('wheel_'+platform+'_'+py for platform in ('linux_x86_64','linux_aarch64','macos_x86_64','macos_aarch64') for py in ('cp311','cp312','cp313','cp314'))


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
        self.assertIn('publish-crates',self.needs(release['github-release']))
        self.assertIn('Crates are separately packaged from source',release['publish-crates'])
        for job,action,path in ((release['github-release'],'softprops/action-gh-release@v3','files: ${{ runner.temp }}/publication/*'),(python['publish-pypi'],'pypa/gh-action-pypi-publish@release/v1','packages-dir: ${{ runner.temp }}/publication')):
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
        self.assertEqual(set(native),{'build','build-ft','qualify','qualify-selected'})
        self.assertEqual(self.needs(native['qualify-selected']), {'build', 'build-ft'})
        self.assertIn('wheel_ft: ${{ steps.identity.outputs.wheel_ft }}', native['build-ft'])
        self.assertIn('python-version: "3.14t"', native['build-ft'])
        self.assertIn('ARTIFACT_ID: ${{ steps.candidate.outputs.artifact-id }}', native['build-ft'])
        selected = native['qualify-selected']
        for value in ('abi: cp312', 'abi: cp314t', 'artifact: wheel\n', 'artifact: wheel_ft\n'):
            self.assertIn(value, selected)
        self.assertIn('PRODUCERS_JSON: ${{ toJSON(needs) }}', selected)
        self.assertIn('artifacts.py download-ids --profile selected-wheels', selected)
        self.assertIn('selected_wheel.py fixture', selected)
        self.assertIn('selected_wheel.py qualify', selected)
        self.assertIn('--fixture-receipt "$RUNNER_TEMP/fixture.json"', selected)
        downloads=[step for step in steps(selected) if 'uses: actions/download-artifact@v4' in step]
        self.assertEqual(len(downloads), 1)
        self.assertIn('artifact-ids: ${{ steps.inventory.outputs[matrix.artifact] }}', downloads[0])
        self.assertNotRegex(downloads[0], r'(?m)^          (name|pattern|run-id|repository|github-token):')
        self.assertIn('\non:\n  workflow_dispatch:\n',manual)
        self.assertNotRegex(manual,r'(?m)^  (push|pull_request|workflow_call):')
        self.assertNotRegex(manual,r'(secrets\.|id-token:|contents: write|gh-action-pypi-publish|action-gh-release|cargo publish)')

    def test_python_314_candidates_run_before_tagged_publication(self):
        workflow = self.workflows['python.yml']
        self.assertIn('  pull_request:', workflow)
        self.assertIn('  workflow_dispatch:', workflow)
        build = jobs(workflow)['build-wheels']
        self.assertIn('      fail-fast: false', build)
        for tag in ('cp311', 'cp312', 'cp313', 'cp314'):
            self.assertIn(f'            tag: {tag}', build)
            self.assertIn(f'            directory: {tag}-{tag}', build)
        self.assertIn('os: ubuntu-24.04-arm', build)
        self.assertIn('image: quay.io/pypa/manylinux2014_aarch64:latest', build)
        mac = next(step for step in steps(build) if 'name: Build with the bound macOS compiler' in step)
        self.assertIn("if: runner.os == 'macOS'", mac)
        self.assertIn('maturin build --locked --release --target "$TARGET"', mac)
        smoke = next(step for step in steps(build) if 'name: Test the sealed cp314 wheel' in step)
        self.assertIn("matrix.python.tag == 'cp314'", smoke)
        self.assertIn('runner.arch', smoke)
        self.assertIn('rustup which --toolchain 1.99.1 cargo', smoke)
        self.assertIn('export RUSTC="$(rustup which --toolchain 1.99.1 rustc)"', smoke)
        self.assertIn('export RUSTDOC="$(rustup which --toolchain 1.99.1 rustdoc)"', smoke)
        self.assertIn('build --locked -p rusty-haystack-server --example python_watch_fixture', smoke)
        self.assertIn('export HAYSTACK_WATCH_FIXTURE=', smoke)
        self.assertIn('target/release-candidate/files/*.whl pytest', smoke)
        self.assertIn('-I -m pytest --import-mode=importlib rusty-haystack/tests -q', smoke)
        self.assertIn('manylinux: auto', build)
        self.assertIn('--locked --release', build)

    def test_crate_publication_order_includes_application_dependency(self):
        publication=steps(jobs(self.workflows['release.yml'])['publish-crates'])
        published=[]
        for index,step in enumerate(publication):
            match=re.search(r'run: cargo publish (?:--[a-z-]+ )*-p ([a-z-]+)',step)
            if match:
                published.append(match[1])
                if match[1] != 'rusty-haystack-cli':
                    self.assertIn('run: sleep 30',publication[index+1])
        self.assertEqual(published,[
            'rusty-haystack-core','rusty-haystack-client','rusty-haystack-app',
            'rusty-haystack-mcp','rusty-haystack-analytics',
            'rusty-haystack-server','rusty-haystack-cli',
        ])
        commands=[step for step in publication if 'cargo publish' in step]
        self.assertEqual(len(commands),len(published))
        for step in commands:
            self.assertRegex(step,r'run: cargo publish --locked -p [a-z-]+\n')
            self.assertIn('CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}',step)
            crate=re.search(r'-p rusty-haystack-([a-z]+)\n',step)[1]
            self.assertIn(f"if: steps.published.outputs.{crate} != 'true'\n",step)
            self.assertIn(f' {crate}',re.search(r'for crate in ([a-z ]+); do',''.join(publication))[0])
        dockerfile=(ROOT/'Dockerfile').read_text()
        for member in ('haystack-app','haystack-mcp','haystack-analytics'):
            self.assertIn(f'COPY {member}/ {member}/',dockerfile)

    def test_crate_publication_order_is_topological(self):
        # cargo resolves every dependency carrying a version, optional or dev, from
        # the registry at publish time, so each must already have been published.
        publication=steps(jobs(self.workflows['release.yml'])['publish-crates'])
        published=[match[1] for step in publication if (match:=re.search(r'run: cargo publish (?:--[a-z-]+ )*-p ([a-z-]+)',step))]
        self.assertTrue(published)
        aliases={}
        for line in (ROOT/'Cargo.toml').read_text().splitlines():
            if (alias:=re.match(r'^(haystack-[a-z]+) = \{ package = "(rusty-haystack-[a-z]+)"',line)):
                aliases[alias[1]]=alias[2]
        manifests={}
        for path in ROOT.glob('*/Cargo.toml'):
            name=re.search(r'(?m)^name = "([a-z-]+)"$',path.read_text())
            if name: manifests[name[1]]=path.read_text()
        for index,crate in enumerate(published):
            internal={aliases[dep] for dep in re.findall(r'(?m)^(haystack-[a-z]+)[ .=]',manifests[crate])}
            self.assertLessEqual(internal,set(published[:index]),crate)
        self.assertEqual(set(published),{name for name,text in manifests.items() if name!='rusty-haystack' and 'publish = false' not in text})

    def test_build_jobs_set_no_environment_the_builder_rejects(self):
        # artifacts.build_inputs refuses any non-empty variable with these prefixes.
        # release.yml once named a matrix flag CROSS_BUILD, which failed the
        # aarch64 Linux archive on the first tag that reached it.
        for file,job_name in (('release.yml','build-binaries'),('python.yml','build-wheels'),('python.yml','build-sdist')):
            job=jobs(self.workflows[file])[job_name]
            names=re.findall(r'(?m)^ {10}([A-Z][A-Z0-9_]*): ',job)
            self.assertTrue(names,(file,job_name))
            for name in names:
                self.assertFalse(name.startswith(('RUSTC','RUSTDOC','MATURIN_','CROSS_')) or name in {'RUSTFLAGS','RUST_TARGET_PATH'},(file,job_name,name))

    def test_release_rehearsal_triggers_cannot_publish(self):
        workflow=self.workflows['release.yml']
        self.assertIn('  pull_request:',workflow)
        self.assertIn('  workflow_dispatch:',workflow)
        release=jobs(workflow)
        guard="if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"
        for name,job in release.items():
            touches_publication=any(marker in job for marker in ('secrets.','environment:','contents: write','cargo publish','action-gh-release'))
            self.assertEqual(touches_publication,name in ('publish-crates','github-release'),name)
            if touches_publication:
                self.assertIn(guard,job)
        self.assertIn("if: startsWith(github.ref, 'refs/tags/v')\n        run: python scripts/release/artifacts.py version --repo \"$GITHUB_WORKSPACE\" --tag \"$GITHUB_REF_NAME\"",release['validate'])

    def test_github_release_waits_for_crates_publication(self):
        release=jobs(self.workflows['release.yml'])
        self.assertEqual(self.needs(release['github-release']),{'validate','qualify-binaries','publish-crates'})
        self.assertIn('--profile publish-binaries',release['github-release'])
        # The publish-binaries profile expects this producer with no outputs.
        self.assertNotIn('\n    outputs:\n',release['publish-crates'])

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
            ('artifact-qualification.yml','needs: [build, build-ft]','needs: build'),
            ('artifact-qualification.yml','artifact-ids: ${{ steps.inventory.outputs[matrix.artifact] }}','name: selected-${{ matrix.abi }}-${{ github.run_attempt }}'),
            ('artifact-qualification.yml','artifact: wheel_ft','artifact: wheel'),
            ('release.yml','artifact-ids: ${{ steps.inventory.outputs.artifact-ids }}','artifact-ids: ${{ steps.inventory.outputs.artifact-ids }}\n          run-id: 123'),
            ('python.yml','proof: ${{ steps.proof.outputs.artifact-id }}','proof: ${{ steps.qualified.outputs.artifact-id }}'),
            ('python.yml','wheel_linux_x86_64_cp311: ${{ steps.identity.outputs.wheel_linux_x86_64_cp311 }}','wheel_linux_x86_64_cp312: ${{ steps.identity.outputs.wheel_linux_x86_64_cp311 }}'),
            ('release.yml','needs: [validate, qualify-binaries, publish-crates]','needs: [validate, qualify-binaries]'),
        ]
        for file,before,after in mutations:
            with self.subTest(file=file,mutation=before):
                self.assertIn(before,self.workflows[file])
                changed=self.workflows | {file:self.workflows[file].replace(before,after)}
                with self.assertRaises(AssertionError): self.check(changed)


if __name__=='__main__':
    unittest.main()
