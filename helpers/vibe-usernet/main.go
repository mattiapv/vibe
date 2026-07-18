// This was entirely vibe-coded. It works, but I'm sure could be better. PRs welcome.

package main

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/containers/gvisor-tap-vsock/pkg/types"
	"github.com/containers/gvisor-tap-vsock/pkg/virtualnetwork"
)

const (
	defaultFD               = 3
	defaultParentLivenessFD = 4

	subnet       = "192.168.5.0/24"
	gatewayIP    = "192.168.5.2"
	guestIP      = "192.168.5.15"
	gatewayMAC   = "5a:94:ef:e4:0c:dd"
	defaultMTU   = 1500
	readyMessage = "ready"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "vibe-usernet: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	fd := flag.Int("fd", defaultFD, "connected VZ datagram file descriptor")
	parentLivenessFD := flag.Int("parent-liveness-fd", defaultParentLivenessFD, "parent liveness file descriptor")
	guestMAC := flag.String("mac", "", "guest MAC address")
	var forwards forwardFlags
	flag.Var(&forwards, "forward", "forward loopback HOST_PORT to GUEST_PORT")
	flag.Parse()

	if flag.NArg() != 0 {
		return fmt.Errorf("unexpected arguments: %s", strings.Join(flag.Args(), " "))
	}
	if _, err := net.ParseMAC(*guestMAC); err != nil {
		return fmt.Errorf("invalid --mac: %w", err)
	}
	forwardMap := make(map[string]string, len(forwards))
	for _, forward := range forwards {
		local := net.JoinHostPort("127.0.0.1", forward.hostPort)
		if _, exists := forwardMap[local]; exists {
			return fmt.Errorf("duplicate --forward host port: %s", forward.hostPort)
		}
		forwardMap[local] = net.JoinHostPort(guestIP, forward.guestPort)
	}

	ctx, stopSignals := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stopSignals()
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	if *parentLivenessFD >= 0 {
		watchParentLiveness(ctx, cancel, *parentLivenessFD)
	}

	conn, err := fileConn(*fd, "vibe-usernet")
	if err != nil {
		return err
	}
	defer conn.Close()

	vn, err := virtualnetwork.New(&types.Configuration{
		Debug:             false,
		MTU:               defaultMTU,
		Subnet:            subnet,
		GatewayIP:         gatewayIP,
		GatewayMacAddress: gatewayMAC,
		DHCPStaticLeases: map[string]string{
			gatewayIP: gatewayMAC,
			guestIP:   *guestMAC,
		},
		Forwards:          forwardMap,
		DNS:               []types.Zone{},
		DNSSearchDomains:  searchDomains(),
		NAT:               map[string]string{gatewayIP: "127.0.0.1"},
		GatewayVirtualIPs: []string{gatewayIP},
	})
	if err != nil {
		return err
	}

	errCh := make(chan error, 1)
	go func() {
		errCh <- vn.AcceptVfkit(ctx, &udpFileConn{Conn: conn})
	}()

	fmt.Fprintln(os.Stdout, readyMessage)

	select {
	case <-ctx.Done():
		return nil
	case err := <-errCh:
		if err == nil || errors.Is(err, net.ErrClosed) || ctx.Err() != nil {
			return nil
		}
		return err
	}
}

type portForward struct {
	hostPort  string
	guestPort string
}

type forwardFlags []portForward

func (forwards *forwardFlags) String() string {
	values := make([]string, 0, len(*forwards))
	for _, forward := range *forwards {
		values = append(values, forward.hostPort+":"+forward.guestPort)
	}
	return strings.Join(values, ",")
}

func (forwards *forwardFlags) Set(value string) error {
	hostPort, guestPort, ok := strings.Cut(value, ":")
	if !ok || hostPort == "" || guestPort == "" || strings.Contains(guestPort, ":") {
		return fmt.Errorf("--forward must have the form HOST_PORT:GUEST_PORT")
	}
	if _, err := net.LookupPort("tcp", hostPort); err != nil {
		return fmt.Errorf("invalid --forward host port %q: %w", hostPort, err)
	}
	if _, err := net.LookupPort("tcp", guestPort); err != nil {
		return fmt.Errorf("invalid --forward guest port %q: %w", guestPort, err)
	}
	*forwards = append(*forwards, portForward{hostPort: hostPort, guestPort: guestPort})
	return nil
}

func fileConn(fd int, name string) (net.Conn, error) {
	file := os.NewFile(uintptr(fd), name)
	if file == nil {
		return nil, fmt.Errorf("invalid fd %d", fd)
	}
	defer file.Close()

	conn, err := net.FileConn(file)
	if err != nil {
		return nil, fmt.Errorf("open fd %d: %w", fd, err)
	}
	return conn, nil
}

func watchParentLiveness(ctx context.Context, cancel context.CancelFunc, fd int) {
	file := os.NewFile(uintptr(fd), "parent-liveness")
	if file == nil {
		cancel()
		return
	}

	go func() {
		defer cancel()
		defer file.Close()
		_, _ = io.Copy(io.Discard, file)
	}()

	go func() {
		<-ctx.Done()
		_ = file.Close()
	}()
}

func searchDomains() []string {
	file, err := os.Open("/etc/resolv.conf")
	if err != nil {
		return nil
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		if after, ok := strings.CutPrefix(scanner.Text(), "search "); ok {
			return strings.Fields(after)
		}
	}
	return nil
}

type udpFileConn struct {
	net.Conn
}

func (conn *udpFileConn) Read(b []byte) (int, error) {
	if err := conn.SetReadDeadline(time.Time{}); err != nil {
		if errors.Is(err, net.ErrClosed) {
			return 0, errors.New("udpFileConn closed")
		}
	}
	return conn.Conn.Read(b)
}
