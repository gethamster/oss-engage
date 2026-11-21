# Workers sharing one queue

Start a local board, then add work:

```sh
engage serve --dir .engage
```

In another terminal:

```sh
export ENGAGE_URL=http://127.0.0.1:4100
engage tasks create work 'Check the first change'
engage tasks create work 'Check the second change'
```

Noninteractive workers claim from the same queue, renew their leases, and complete or fail the
task according to the command's exit status:

```sh
engage --as worker-one tasks run work/ --ready -- /usr/bin/true
engage --as worker-two tasks run work/ --ready -- /usr/bin/true
```

Run the workers concurrently from separate terminals. Replace `/usr/bin/true`
with your actual test command or agent launcher. The command runner stops its
child if renewal fails, so a process cannot keep working after losing its lease.
Open `engage inspect` to watch threads and proof. See `engage tasks --help` and
`engage tasks run --help` for lease and wait controls.

For ongoing model-driven work, use the native Adam extension rather than a
shell command that invents a second session/task store. The extension handles
shared task capabilities privately and preserves the thread's session.

On Unix, lease loss stops the worker process group and its descendants. Other
platforms stop the immediate child. Standard input is closed; pass worker input
through arguments, files, or the Board API.
