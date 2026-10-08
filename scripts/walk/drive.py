#!/usr/bin/env python3
"""Drive a debug build of omarepos that nobody is looking at.

The window is started on a hidden Hyprland workspace, against fixture
repositories and a scratch configuration, and everything after that goes over
D-Bus to the actions `src/ui/debug.rs` adds: nothing passes through the
compositor, so the desktop it runs on is not disturbed.

    drive.py start | stop | alive
    drive.py keys KEY...        e.g. keys ctrl+f Return shift+question
    drive.py type TEXT
    drive.py press LABEL        "!LABEL" reaches a button that is not on screen
    drive.py row "Title=value"  a settings row: on/off/toggle, a number, text
    drive.py pick LIST:ROW      select a list row; "LIST:ROW!" activates it
    drive.py choose N           the first drop-down on screen
    drive.py respond ID         answer a question dialog (save, remove, cancel)
    drive.py paint              run every draw function on screen
    drive.py dump [FILTER]      what is on screen, as text
    drive.py dump-all [FILTER]  everything, on screen or not
    drive.py act NAME [ARG]     any window action
"""
import contextlib, io, json, os, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
W = os.environ.get('OMAREPOS_WALK_DIR', os.path.join(REPO, 'target', 'walk'))
BIN = os.path.join(REPO, 'target', 'debug', 'omarepos')
PIDFILE = os.path.join(W, 'pid')
OBJECT = '/se/alphadev/Omarepos/window/1'

# The window's accelerators are actions, and are activated as such.
ACCELS = {'ctrl+r': 'refresh', 'F5': 'refresh', 'ctrl+shift+r': 'fetch', 'Escape': 'escape',
          'F1': 'keyboard', 'ctrl+a': 'select-all', 'ctrl+f': 'search', 'ctrl+g': 'contributions',
          'ctrl+l': 'focus-grid', 'ctrl+shift+f': 'search-commits', 'ctrl+comma': 'settings',
          'ctrl+shift+a': 'clear-selection'}


def sh(*args):
    return subprocess.run(args, capture_output=True, text=True)


def ours(pid):
    """Whether this process is a walk's instance: by what it was started with, not by its
    path, which reads "(deleted)" once the binary has been rebuilt under it."""
    try:
        exe = os.readlink(f'/proc/{pid}/exe')
        env = open(f'/proc/{pid}/environ').read()
    except OSError:
        return False
    return 'omarepos' in exe and f'XDG_CONFIG_HOME={W}/config' in env


def mine():
    pid = int(open(PIDFILE).read())
    if not ours(pid) or os.readlink(f'/proc/{pid}/exe') != BIN:
        raise SystemExit('dead')
    return pid


def clients():
    return json.loads(sh('hyprctl', 'clients', '-j').stdout or '[]')


def stop_all():
    for name in os.listdir('/proc'):
        if name.isdigit() and ours(int(name)):
            os.kill(int(name), 15)


def start():
    stop_all()
    time.sleep(0.4)
    before = {c['pid'] for c in clients()}
    env = (f'env XDG_CONFIG_HOME={W}/config XDG_STATE_HOME={W}/state GIT_CONFIG_GLOBAL={W}/gitconfig '
           f'GIT_CONFIG_NOSYSTEM=1 GDK_DEBUG=no-portals OMAREPOS_DUMP={W}/dump.txt')
    command = f"sh -c '{env} {BIN} --root {W}/fleet > {W}/app.log 2>&1'"
    sh('hyprctl', 'dispatch', 'hl.dsp.exec_cmd("%s", { workspace = "special:omarepos-walk silent" })' % command)
    for _ in range(50):
        time.sleep(0.2)
        new = [c for c in clients() if c['pid'] not in before and c['class'] == 'se.alphadev.Omarepos']
        if new:
            open(PIDFILE, 'w').write(str(new[0]['pid']))
            mine()
            settle()
            return
    raise SystemExit('did not start')


def settle():
    """Wait for the window to be given its real size. It is laid out first at its default
    width and then at the workspace's, and with no frames arriving a change of breakpoint
    between the two leaves the content off screen until the next dump puts it back."""
    last = None
    for _ in range(40):
        time.sleep(0.25)
        buf = io.StringIO()
        try:
            with contextlib.redirect_stdout(buf):
                dump('width:')
        except (SystemExit, OSError):
            continue
        now = buf.getvalue().strip()
        if now and now == last and 'width: 1000' not in now:
            return
        last = now


def bus():
    pid = mine()
    for line in sh('busctl', '--user', 'list', '--no-legend').stdout.splitlines():
        field = line.split()
        if len(field) > 1 and field[0].startswith(':') and field[1] == str(pid):
            return field[0]
    raise SystemExit('no bus name')


def act(name, arg=None):
    param = '[]' if arg is None else "[<'%s'>]" % arg.replace("\\", "\\\\").replace("'", "\\'")
    done = sh('gdbus', 'call', '--session', '--dest', bus(), '--object-path', OBJECT,
              '--method', 'org.gtk.Actions.Activate', name, param, '{}')
    if done.stdout.strip() != '()':
        print(done.stdout.strip() or done.stderr.strip())


def keys(names):
    for name in names:
        if name in ACCELS:
            act(ACCELS[name])
        else:
            act('key', name)
        time.sleep(0.25)


def dump(needle=None, everything=False):
    path = os.path.join(W, 'dump.txt')
    with contextlib.suppress(FileNotFoundError):
        os.remove(path)
    act('dump-all' if everything else 'dump')
    for _ in range(20):
        if os.path.exists(path):
            break
        time.sleep(0.05)
    for line in open(path).read().replace(W, 'W').splitlines():
        if needle is None:
            print(line)
        elif needle.lower() in line.lower():
            print(line.strip())


def main(argv):
    if not argv:
        raise SystemExit(__doc__)
    cmd, rest = argv[0], argv[1:]
    first = rest[0] if rest else None
    if cmd == 'start':
        start()
    elif cmd == 'stop':
        stop_all()
    elif cmd == 'alive':
        print('alive', mine())
    elif cmd == 'keys':
        keys(rest)
    elif cmd == 'dump':
        dump(first)
    elif cmd == 'dump-all':
        dump(first, everything=True)
    elif cmd == 'act':
        act(*rest[:2])
    elif cmd in ('type', 'row', 'pick', 'choose', 'respond', 'press'):
        act(cmd, first)
    elif cmd == 'paint':
        act('paint')
    else:
        raise SystemExit(__doc__)


if __name__ == '__main__':
    main(sys.argv[1:])
