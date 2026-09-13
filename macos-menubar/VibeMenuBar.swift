import AppKit
import Foundation

@main
struct VibeMenuBarApp {
    static func main() {
        let application = NSApplication.shared
        application.setActivationPolicy(.accessory)

        let delegate = VibeMenuBarDelegate()
        application.delegate = delegate
        application.run()
    }
}

final class VibeMenuBarDelegate: NSObject, NSApplicationDelegate {
    private let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let statusMenuItem = NSMenuItem(title: "Checking Vibe…", action: nil, keyEquivalent: "")
    private let startMenuItem = NSMenuItem(title: "Start Vibe", action: #selector(startMainVM), keyEquivalent: "")
    private let stopMenuItem = NSMenuItem(title: "Stop Vibe", action: #selector(stopMainVM), keyEquivalent: "")
    private var mainVMID: String?
    private var refreshTimer: Timer?
    private var settingsWindowController: VibeSettingsWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.addItem(statusMenuItem)
        menu.addItem(.separator())
        menu.addItem(startMenuItem)
        menu.addItem(stopMenuItem)
        menu.addItem(.separator())
        menu.addItem(withTitle: "Refresh", action: #selector(refreshStatus), keyEquivalent: "r")
        menu.addItem(withTitle: "Open Settings", action: #selector(openSettings), keyEquivalent: ",")
        menu.addItem(.separator())
        menu.addItem(withTitle: "Quit Vibe", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")

        startMenuItem.target = self
        stopMenuItem.target = self
        statusItem.menu = menu
        statusItem.button?.image = vibeMenuBarImage(opacity: 0.5)

        refreshStatus()
        refreshTimer = Timer.scheduledTimer(
            timeInterval: 5,
            target: self,
            selector: #selector(refreshStatus),
            userInfo: nil,
            repeats: true
        )
    }

    @objc private func refreshStatus() {
        runVibe(arguments: ["ssh", "--list"]) { [weak self] result in
            guard let self else { return }

            switch result {
            case .success(let output):
                let mainRecord = self.mainRecord(from: output)
                self.mainVMID = mainRecord?.id
                let running = mainRecord?.status == "running"

                self.statusItem.button?.image = self.vibeMenuBarImage(opacity: running ? 1 : 0.5)
                self.statusItem.button?.contentTintColor = nil
                self.statusMenuItem.image = self.statusDot(color: running ? .systemGreen : .systemRed)
                self.statusMenuItem.title = running ? "Vibe is running" : "Vibe is not running"
                self.startMenuItem.isEnabled = !running
                self.stopMenuItem.isEnabled = running

            case .failure:
                self.mainVMID = nil
                self.statusItem.button?.image = NSImage(systemSymbolName: "exclamationmark.triangle", accessibilityDescription: "Vibe unavailable")
                self.statusItem.button?.contentTintColor = .darkGray
                self.statusMenuItem.image = self.statusDot(color: .systemRed)
                self.statusMenuItem.title = "Vibe is not running"
                self.startMenuItem.isEnabled = false
                self.stopMenuItem.isEnabled = false
            }
        }
    }

    @objc private func startMainVM() {
        startMenuItem.isEnabled = false
        statusMenuItem.title = "Starting Vibe…"
        statusItem.button?.image = vibeMenuBarImage()
        statusItem.button?.contentTintColor = nil

        runVibe(arguments: ["ssh", "--main", "--no-mount"]) { [weak self] _ in
            self?.refreshStatus()
        }
    }

    @objc private func stopMainVM() {
        guard let mainVMID else { return }

        stopMenuItem.isEnabled = false
        statusMenuItem.title = "Stopping Vibe…"
        statusItem.button?.image = vibeMenuBarImage(opacity: 0.5)
        statusItem.button?.contentTintColor = nil
        runVibe(arguments: ["ssh", "--stop", mainVMID]) { [weak self] _ in
            self?.refreshStatus()
        }
    }

    @objc private func openSettings() {
        if let settingsWindowController {
            NSApp.activate(ignoringOtherApps: true)
            settingsWindowController.showWindow(nil)
            settingsWindowController.window?.makeKeyAndOrderFront(nil)
            return
        }

        let windowController = VibeSettingsWindowController()
        settingsWindowController = windowController
        NSApp.activate(ignoringOtherApps: true)
        windowController.showWindow(nil)
    }

    private func mainRecord(from output: String) -> (id: String, status: String)? {
        for line in output.split(whereSeparator: \.isNewline).dropFirst() {
            let columns = line.split(whereSeparator: \.isWhitespace)
            guard columns.count >= 4, columns[3] == "main" else { continue }
            return (String(columns[0]), String(columns[1]))
        }
        return nil
    }

    private func vibeMenuBarImage(opacity: CGFloat = 1) -> NSImage? {
        guard let url = Bundle.main.url(forResource: "v-menubar", withExtension: "png"),
              let sourceImage = NSImage(contentsOf: url) else {
            return nil
        }

        let size = NSSize(width: 18, height: 18)
        let image = NSImage(size: size)
        let bounds = NSRect(origin: .zero, size: size)
        image.lockFocus()
        sourceImage.draw(in: bounds, from: .zero, operation: .sourceOver, fraction: opacity)
        image.unlockFocus()
        image.isTemplate = true
        image.accessibilityDescription = "Vibe VM"
        return image
    }

    private func statusDot(color: NSColor) -> NSImage {
        let size = NSSize(width: 8, height: 8)
        let image = NSImage(size: size)
        image.lockFocus()
        color.setFill()
        NSBezierPath(ovalIn: NSRect(origin: .zero, size: size)).fill()
        image.unlockFocus()
        image.isTemplate = false
        return image
    }

    private func runVibe(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: VibeMenuBarConfig.executablePath)
            process.arguments = arguments
            process.standardInput = FileHandle.nullDevice

            let output = Pipe()
            process.standardOutput = output
            process.standardError = output

            do {
                try process.run()
                process.waitUntilExit()
                let data = output.fileHandleForReading.readDataToEndOfFile()
                let text = String(decoding: data, as: UTF8.self)
                if process.terminationStatus == 0 {
                    DispatchQueue.main.async { completion(.success(text)) }
                } else {
                    DispatchQueue.main.async {
                        completion(.failure(VibeError.commandFailed(text.isEmpty ? "Exit status \(process.terminationStatus)" : text)))
                    }
                }
            } catch {
                DispatchQueue.main.async { completion(.failure(error)) }
            }
        }
    }
}

private enum VibeError: LocalizedError {
    case commandFailed(String)

    var errorDescription: String? {
        switch self {
        case .commandFailed(let message): return message.trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }
}
