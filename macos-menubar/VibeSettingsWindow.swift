import AppKit
import Foundation
import ServiceManagement

final class VibeSettingsWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    private let tableView = NSTableView()
    private let statusLabel = NSTextField(labelWithString: "")
    private let openAtLoginCheckbox = NSButton(checkboxWithTitle: "Open Vibe at login", target: nil, action: nil)
    private var folders: [String] = []
    private var comments: [String] = []

    init() {
        let contentView = NSView()
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 620, height: 380),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Vibe Settings"
        window.contentView = contentView
        super.init(window: window)

        configureContent(in: contentView)
        loadFolders()
        refreshOpenAtLoginStatus()
        window.center()
    }

    required init?(coder: NSCoder) {
        nil
    }

    private func configureContent(in contentView: NSView) {
        let descriptionLabel = NSTextField(wrappingLabelWithString: "Folders mounted in the Vibe main VM")
        descriptionLabel.font = .systemFont(ofSize: 16, weight: .semibold)

        let tableColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("folder"))
        tableColumn.title = "Host folder"
        tableColumn.resizingMask = .autoresizingMask
        tableView.addTableColumn(tableColumn)
        tableView.headerView = nil
        tableView.delegate = self
        tableView.dataSource = self
        tableView.usesAlternatingRowBackgroundColors = true

        let scrollView = NSScrollView()
        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.borderType = .bezelBorder

        let addButton = NSButton(title: "Add Folder…", target: self, action: #selector(addFolder))
        let removeButton = NSButton(title: "Remove", target: self, action: #selector(removeSelectedFolder))
        let reloadButton = NSButton(title: "Reload", target: self, action: #selector(loadFolders))
        let saveButton = NSButton(title: "Save", target: self, action: #selector(saveFolders))
        saveButton.keyEquivalent = "\r"

        openAtLoginCheckbox.target = self
        openAtLoginCheckbox.action = #selector(updateOpenAtLogin)

        statusLabel.textColor = .secondaryLabelColor
        statusLabel.lineBreakMode = .byTruncatingMiddle

        let buttonRow = NSStackView(views: [addButton, removeButton, NSView(), reloadButton, saveButton])
        buttonRow.orientation = .horizontal
        buttonRow.spacing = 8
        buttonRow.setHuggingPriority(.defaultLow, for: .horizontal)

        let stack = NSStackView(views: [descriptionLabel, scrollView, buttonRow, openAtLoginCheckbox, statusLabel])
        stack.orientation = .vertical
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 20),
            stack.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -20),
            scrollView.heightAnchor.constraint(greaterThanOrEqualToConstant: 180),
        ])
    }

    @objc private func addFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        panel.prompt = "Add"

        guard panel.runModal() == .OK else { return }
        for url in panel.urls {
            let path = url.standardizedFileURL.path
            if !folders.contains(path) {
                folders.append(path)
            }
        }
        folders.sort()
        tableView.reloadData()
    }

    @objc private func removeSelectedFolder() {
        let row = tableView.selectedRow
        guard folders.indices.contains(row) else { return }
        folders.remove(at: row)
        tableView.reloadData()
    }

    @objc private func loadFolders() {
        do {
            let content = try String(contentsOf: mountedFoldersURL, encoding: .utf8)
            comments = content.split(whereSeparator: \.isNewline).compactMap { line in
                let value = String(line)
                return value.hasPrefix("#") ? value : nil
            }
            folders = content.split(whereSeparator: \.isNewline).compactMap { line in
                let value = String(line)
                return value.isEmpty || value.hasPrefix("#") ? nil : value
            }
            tableView.reloadData()
            statusLabel.stringValue = "Loaded \(folders.count) folder(s)."
        } catch CocoaError.fileNoSuchFile {
            comments = defaultComments
            folders = []
            tableView.reloadData()
            statusLabel.stringValue = "No mounted-folders.txt yet. Save to create it."
        } catch {
            statusLabel.stringValue = "Could not load folders: \(error.localizedDescription)"
        }
    }

    @objc private func saveFolders() {
        do {
            try FileManager.default.createDirectory(
                at: mountedFoldersURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            let content = (comments + folders).joined(separator: "\n") + "\n"
            try content.write(to: mountedFoldersURL, atomically: true, encoding: .utf8)
            statusLabel.stringValue = "Saved. Restart Vibe to apply mount changes."
        } catch {
            statusLabel.stringValue = "Could not save folders: \(error.localizedDescription)"
        }
    }

    @objc private func updateOpenAtLogin() {
        guard #available(macOS 13.0, *) else {
            openAtLoginCheckbox.state = .off
            statusLabel.stringValue = "Open at login requires macOS 13 or later."
            return
        }

        do {
            let service = SMAppService.mainApp
            if openAtLoginCheckbox.state == .on {
                try service.register()
            } else {
                try service.unregister()
            }
            refreshOpenAtLoginStatus()
        } catch {
            refreshOpenAtLoginStatus()
            statusLabel.stringValue = "Could not update Open at Login: \(error.localizedDescription)"
        }
    }

    private func refreshOpenAtLoginStatus() {
        guard #available(macOS 13.0, *) else {
            openAtLoginCheckbox.state = .off
            openAtLoginCheckbox.isEnabled = false
            openAtLoginCheckbox.toolTip = "Requires macOS 13 or later"
            return
        }

        openAtLoginCheckbox.isEnabled = true
        openAtLoginCheckbox.state = SMAppService.mainApp.status == .enabled ? .on : .off
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        folders.count
    }

    func tableView(
        _ tableView: NSTableView,
        viewFor tableColumn: NSTableColumn?,
        row: Int
    ) -> NSView? {
        let identifier = NSUserInterfaceItemIdentifier("folderCell")
        let cell = tableView.makeView(withIdentifier: identifier, owner: self) as? NSTableCellView
            ?? NSTableCellView()
        cell.identifier = identifier

        let textField = cell.textField ?? NSTextField(labelWithString: "")
        if cell.textField == nil {
            textField.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(textField)
            NSLayoutConstraint.activate([
                textField.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 6),
                textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -6),
                textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            cell.textField = textField
        }
        textField.stringValue = folders[row]
        return cell
    }

    private var mountedFoldersURL: URL {
        let cacheDirectory = ProcessInfo.processInfo.environment["XDG_CACHE_HOME"]
            .map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".cache", isDirectory: true)
        return cacheDirectory
            .appendingPathComponent("vibe", isDirectory: true)
            .appendingPathComponent("main", isDirectory: true)
            .appendingPathComponent("mounted-folders.txt")
    }

    private var defaultComments: [String] {
        [
            "# One absolute host folder path per line.",
            "# Changes apply after the main VM is restarted.",
        ]
    }
}
