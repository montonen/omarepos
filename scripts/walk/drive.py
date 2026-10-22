#!/usr/bin/env python3
"""Drive a debug build of omarepos that nobody is looking at.

The window is started on a hidden Hyprland workspace, against fixture
repositories and a scratch configuration, and everything after that goes over
D-Bus to the actions `src/ui/debug.rs` adds: nothing passes through the
compositor, so the desktop it runs on is not disturbed.

    drive.py start | stop | alive
    drive.py start-offscreen [WxH]  on a display of its own instead (see below)
    drive.py keys KEY...        e.g. keys ctrl+f Return shift+question
    drive.py type TEXT
    drive.py press LABEL        "!LABEL" reaches a button that is not on screen
    drive.py row "Title=value"  a settings row: on/off/toggle, a number, text
    drive.py pick LIST:ROW      select a list row; "LIST:ROW!" activates it
    drive.py choose N           the first drop-down on screen; "Tooltip=N" names one
    drive.py respond ID         answer a question dialog (save, remove, cancel)
    drive.py paint              run every draw function on screen
    drive.py dump [FILTER]      what is on screen, as text
    drive.py dump-all [FILTER]  everything, on screen or not
    drive.py resize WxH         offscreen only: a tiling compositor decides for itself
    drive.py shot FILE.png [2]  a picture of the window as it is laid out; 2 for twice the size
    drive.py split N            the divider of a split page, N pixels from its top
    drive.py point N            the pointer over card N of the grid; "off" takes it away
    drive.py wheel              the wheel, turned over the grid
    drive.py act NAME [ARG]     any window action

A hidden workspace sends a window no frames, so nothing there is laid out or
drawn unless asked, and a change of width never finishes. `start-offscreen`
runs the window on GTK's Broadway display instead, with its own daemon and no
port open: frames arrive, sizes are real and can be chosen, and Hyprland is
not involved at all. Given a size, it is started at that size and has come to
rest there when this returns. Everything else is driven the same way.

OMAREPOS_WALK_DIR is where the scratch configuration and state are (target/walk
unless set), and OMAREPOS_WALK_ROOT the directory scanned (fleet/ in there).
Set to nothing, the roots are the scratch configuration's own, and the window
is an ordinary one and not a one-off look: it has a refresh, and says so.
"""
import contextlib, hashlib, io, json, os, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
W = os.environ.get('OMAREPOS_WALK_DIR', os.path.join(REPO, 'target', 'walk'))
ROOT = os.environ.get('OMAREPOS_WALK_ROOT', os.path.join(W, 'fleet'))
WHERE = ['--root', ROOT] if ROOT else []
BIN = os.path.join(REPO, 'target', 'debug', 'omarepos')
PIDFILE = os.path.join(W, 'pid')
OBJECT = '/se/montonen/Omarepos/window/1'
# Offscreen, GTK does not put the window on the bus. The application is there
# on any display, and a debug build's `app.window` passes an action on.
APP_OBJECT = '/se/montonen/Omarepos'
OFFSCREEN = os.path.join(W, 'offscreen')
DAEMON_PID = os.path.join(W, 'broadway.pid')
DISPLAY = ':9'

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
    with contextlib.suppress(FileNotFoundError):
        os.remove(OFFSCREEN)


def http_socket():
    """Where the display daemon listens for a browser: a socket and not a port, so that
    nothing is opened to the network. Beside everything else of the walk's, unless that
    is too deep: a socket's path holds 107 bytes, the daemon cuts a longer one short
    without a word, and what it then leaves behind is not what gets removed, so that
    the next start finds the address in use. Then it goes in the runtime directory,
    under a name made from the walk's own directory."""
    path = os.path.join(W, 'broadway-http.sock')
    runtime = os.environ.get('XDG_RUNTIME_DIR')
    if len(os.fsencode(path)) > 100 and runtime:
        path = os.path.join(runtime, 'omarepos-walk-%s.sock' % hashlib.sha1(os.fsencode(W)).hexdigest()[:12])
    return path


def daemon():
    """The pid of this walk's display daemon, if it is running: by what it was started
    with, like the window."""
    try:
        pid = int(open(DAEMON_PID).read())
        cmdline = open(f'/proc/{pid}/cmdline').read()
    except (OSError, ValueError):
        return None
    return pid if 'gtk4-broadwayd' in cmdline and http_socket() in cmdline else None


def stop_daemon():
    pid = daemon()
    if pid:
        os.kill(pid, 15)
        with contextlib.suppress(OSError):
            os.remove(http_socket())
        # It leaves the socket windows reach it by behind: display N is N + 1 there.
        runtime = os.environ.get('XDG_RUNTIME_DIR')
        if runtime:
            with contextlib.suppress(OSError):
                os.remove(os.path.join(runtime, 'broadway%d.socket' % (int(DISPLAY[1:]) + 1)))


def start_offscreen(size=None):
    stop_all()
    time.sleep(0.4)
    if not daemon():
        http = http_socket()
        with contextlib.suppress(FileNotFoundError):
            os.remove(http)
        server = subprocess.Popen(['gtk4-broadwayd', '--unixsocket', http, DISPLAY],
                                  stdout=open(os.path.join(W, 'broadway.log'), 'w'),
                                  stderr=subprocess.STDOUT, start_new_session=True)
        open(DAEMON_PID, 'w').write(str(server.pid))
        time.sleep(0.6)
    # Drawn with cairo. GTK's own renderer for this display sends the daemon each
    # frame as a tree of nodes, naming those it keeps from the frame before, and
    # with GTK 4.24 it can name one the daemon does not have: the daemon stops on
    # an assertion ("decode_nodes: assertion failed: (node != NULL)"), the window
    # goes with it, and a walk says omarepos stopped. The walk met it at the same
    # step every time, the grid filled again under a toast after Contributions
    # had been opened and closed. With cairo a frame is one picture, and names
    # nothing. Nothing here goes by how the daemon was sent a frame: `shot` and
    # `render` draw with a renderer of their own.
    env = dict(os.environ, GDK_BACKEND='broadway', BROADWAY_DISPLAY=DISPLAY, GSK_RENDERER='cairo',
               XDG_CONFIG_HOME=f'{W}/config', XDG_STATE_HOME=f'{W}/state',
               GIT_CONFIG_GLOBAL=f'{W}/gitconfig', GIT_CONFIG_NOSYSTEM='1',
               GDK_DEBUG='no-portals', OMAREPOS_DUMP=f'{W}/dump.txt')
    app = subprocess.Popen([BIN, *WHERE], env=env, start_new_session=True,
                           stdout=open(os.path.join(W, 'app.log'), 'w'), stderr=subprocess.STDOUT)
    open(PIDFILE, 'w').write(str(app.pid))
    open(OFFSCREEN, 'w').write(DISPLAY)
    for _ in range(50):
        time.sleep(0.2)
        with contextlib.suppress(SystemExit):
            bus()
            settle()
            break
    else:
        raise SystemExit('did not start')
    if size:
        come_to(size)


def come_to(size):
    """Give a window on a display of its own a size, and wait until it is that wide and
    at rest. A change of width across a breakpoint takes the content off screen until
    a later frame, and frames come about one a second here; nothing is drawn while
    nothing changes, so two seconds with no new frame is a window at rest."""
    act('resize', size)
    width = 'width: %s ' % size.split('x')[0]
    last, still = None, 0
    for _ in range(120):
        time.sleep(0.25)
        now = (look('width:'), look('frames:'))
        still = still + 1 if now == last and width in (now[0] or '') else 0
        if still >= 8:
            return
        last = now
    raise SystemExit(f'did not come to {size}: {look("width:")}')


def start():
    stop_all()
    time.sleep(0.4)
    before = {c['pid'] for c in clients()}
    env = (f'env XDG_CONFIG_HOME={W}/config XDG_STATE_HOME={W}/state GIT_CONFIG_GLOBAL={W}/gitconfig '
           f'GIT_CONFIG_NOSYSTEM=1 GDK_DEBUG=no-portals OMAREPOS_DUMP={W}/dump.txt')
    command = f"sh -c '{env} {BIN} {' '.join(WHERE)} > {W}/app.log 2>&1'"
    sh('hyprctl', 'dispatch', 'hl.dsp.exec_cmd("%s", { workspace = "special:omarepos-walk silent" })' % command)
    for _ in range(50):
        time.sleep(0.2)
        new = [c for c in clients() if c['pid'] not in before and c['class'] == 'se.montonen.Omarepos']
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
        now = look('width:')
        if now and now == last and 'width: 1000' not in now:
            return
        last = now


def look(needle):
    """What `dump NEEDLE` would print, as a string; None while the window cannot say."""
    buf = io.StringIO()
    try:
        with contextlib.redirect_stdout(buf):
            dump(needle)
    except (SystemExit, OSError):
        return None
    return buf.getvalue().strip()


def bus():
    pid = mine()
    for line in sh('busctl', '--user', 'list', '--no-legend').stdout.splitlines():
        field = line.split()
        if len(field) > 1 and field[0].startswith(':') and field[1] == str(pid):
            return field[0]
    raise SystemExit('no bus name')


def act(name, arg=None):
    quoted = None if arg is None else "'%s'" % arg.replace("\\", "\\\\").replace("'", "\\'")
    if os.path.exists(OFFSCREEN):
        path, action = APP_OBJECT, 'window'
        param = "[<['%s'%s]>]" % (name, '' if quoted is None else ', ' + quoted)
    else:
        path, action = OBJECT, name
        param = '[]' if quoted is None else '[<%s>]' % quoted
    done = sh('gdbus', 'call', '--session', '--dest', bus(), '--object-path', path,
              '--method', 'org.gtk.Actions.Activate', action, param, '{}')
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
    elif cmd == 'start-offscreen':
        start_offscreen(first)
    elif cmd == 'stop':
        stop_all()
        stop_daemon()
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
    elif cmd == 'shot':
        scale = rest[1] + ':' if len(rest) > 1 else ''
        act('shot', scale + os.path.abspath(first))
    elif cmd in ('type', 'row', 'pick', 'choose', 'respond', 'press', 'resize', 'split', 'point'):
        act(cmd, first)
    elif cmd in ('paint', 'wheel'):
        act(cmd)
    else:
        raise SystemExit(__doc__)


if __name__ == '__main__':
    main(sys.argv[1:])
