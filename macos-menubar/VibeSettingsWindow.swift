import AppKit
import Foundation
import ServiceManagement

final class VibeSettingsWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    private let tableView = NSTableView()
    private let statusLabel = NSTextField(labelWithString: "")
    private let openAtLoginCheckbox = NSButton(checkboxWithTitle: "Open Vibe at login", target: nil, action: nil)
    private let vmTableView = NSTableView()
    private let vmStatusLabel = NSTextField(labelWithString: "")
    private let deleteVMButton = NSButton(title: "Delete VM…", target: nil, action: nil)
    private var folders: [String] = []
    private var comments: [String] = []
    private var vmRecords: [VMRecord] = []
    private var deletingVM = false

    private struct VMRecord: Decodable {
        let folder_path: String
        let created_at: String
    }

    private struct VMRegistry: Decodable {
        let vm_folders: [VMRecord]
    }

    init() {
        let contentView = NSView()
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 780, height: 500),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Vibe Settings"
        window.contentMinSize = NSSize(width: 680, height: 460)
        window.contentView = contentView
        super.init(window: window)

        configureContent(in: contentView)
        loadFolders()
        loadVMs()
        refreshOpenAtLoginStatus()
        window.center()
    }

    required init?(coder: NSCoder) {
        nil
    }

    private func configureContent(in contentView: NSView) {
        let tabs = NSTabView()
        tabs.translatesAutoresizingMaskIntoConstraints = false
        let foldersTab = NSTabViewItem(identifier: "folders")
        foldersTab.label = "Mounted Folders"
        let foldersView = NSView()
        foldersTab.view = foldersView
        configureFolders(in: foldersView)
        tabs.addTabViewItem(foldersTab)

        let vmsTab = NSTabViewItem(identifier: "vms")
        vmsTab.label = "Saved VMs"
        let vmsView = NSView()
        vmsTab.view = vmsView
        configureVMs(in: vmsView)
        tabs.addTabViewItem(vmsTab)

        contentView.addSubview(tabs)
        NSLayoutConstraint.activate([
            tabs.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 12),
            tabs.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -12),
            tabs.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 12),
            tabs.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -12),
        ])
    }

    private func configureFolders(in contentView: NSView) {
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

    private func configureVMs(in contentView: NSView) {
        let descriptionLabel = NSTextField(wrappingLabelWithString: "VMs tracked by vibe ls")
        descriptionLabel.font = .systemFont(ofSize: 16, weight: .semibold)

        let dateColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("createdAt"))
        dateColumn.title = "Created"
        dateColumn.width = 105
        vmTableView.addTableColumn(dateColumn)
        let folderColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("vmFolder"))
        folderColumn.title = "Project folder"
        folderColumn.resizingMask = .autoresizingMask
        vmTableView.addTableColumn(folderColumn)
        vmTableView.delegate = self
        vmTableView.dataSource = self
        vmTableView.usesAlternatingRowBackgroundColors = true

        let scrollView = NSScrollView()
        scrollView.documentView = vmTableView
        scrollView.hasVerticalScroller = true
        scrollView.borderType = .bezelBorder

        deleteVMButton.target = self
        deleteVMButton.action = #selector(deleteSelectedVM)
        deleteVMButton.isEnabled = false
        let reloadButton = NSButton(title: "Reload", target: self, action: #selector(loadVMs))
        let buttonRow = NSStackView(views: [deleteVMButton, NSView(), reloadButton])
        buttonRow.orientation = .horizontal
        buttonRow.spacing = 8
        buttonRow.setHuggingPriority(.defaultLow, for: .horizontal)

        vmStatusLabel.textColor = .secondaryLabelColor
        vmStatusLabel.lineBreakMode = .byTruncatingMiddle
        let stack = NSStackView(views: [descriptionLabel, scrollView, buttonRow, vmStatusLabel])
        stack.orientation = .vertical
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        contentView.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 12),
            stack.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -20),
            scrollView.heightAnchor.constraint(greaterThanOrEqualToConstant: 180),
        ])
    }

    @objc func loadVMs() {
        do {
            let data = try Data(contentsOf: vmRegistryURL)
            if String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                vmRecords = []
            } else {
                vmRecords = try JSONDecoder().decode(VMRegistry.self, from: data).vm_folders
            }
            vmRecords.sort {
                $0.created_at == $1.created_at
                    ? $0.folder_path < $1.folder_path
                    : $0.created_at > $1.created_at
            }
            vmTableView.reloadData()
            deleteVMButton.isEnabled = !deletingVM && vmRecords.indices.contains(vmTableView.selectedRow)
            vmStatusLabel.stringValue = vmRecords.isEmpty ? "No tracked VMs." : "Loaded \(vmRecords.count) VM(s)."
        } catch CocoaError.fileReadNoSuchFile {
            vmRecords = []
            vmTableView.reloadData()
            deleteVMButton.isEnabled = false
            vmStatusLabel.stringValue = "No tracked VMs."
        } catch {
            vmRecords = []
            vmTableView.reloadData()
            deleteVMButton.isEnabled = false
            vmStatusLabel.stringValue = "Could not load VMs: \(error.localizedDescription)"
        }
    }

    @objc private func deleteSelectedVM() {
        let row = vmTableView.selectedRow
        guard !deletingVM, vmRecords.indices.contains(row) else { return }
        let record = vmRecords[row]
        let alert = NSAlert()
        alert.messageText = "Delete saved VM?"
        alert.informativeText = "This removes the tracked entry and deletes the .vibe folder in:\n\(record.folder_path)\n\nThe project folder itself is kept."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Delete VM")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        deletingVM = true
        deleteVMButton.isEnabled = false
        vmStatusLabel.stringValue = "Deleting VM…"
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: VibeMenuBarConfig.executablePath)
            process.arguments = ["ls", "--delete-folder", record.folder_path]
            process.standardInput = FileHandle.nullDevice
            let output = Pipe()
            process.standardOutput = output
            process.standardError = output
            do {
                try process.run()
                let data = output.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                let message = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
                DispatchQueue.main.async {
                    self?.deletingVM = false
                    self?.loadVMs()
                    self?.vmStatusLabel.stringValue = message.isEmpty
                        ? "VM command exited with status \(process.terminationStatus)."
                        : message.replacingOccurrences(of: "\n", with: " · ")
                    self?.vmStatusLabel.toolTip = message
                }
            } catch {
                DispatchQueue.main.async {
                    self?.deletingVM = false
                    self?.vmStatusLabel.stringValue = "Could not delete VM: \(error.localizedDescription)"
                    if let self {
                        self.deleteVMButton.isEnabled = self.vmRecords.indices.contains(self.vmTableView.selectedRow)
                    }
                }
            }
        }
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
        tableView === vmTableView ? vmRecords.count : folders.count
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard notification.object as? NSTableView === vmTableView else { return }
        deleteVMButton.isEnabled = !deletingVM && vmRecords.indices.contains(vmTableView.selectedRow)
    }

    func tableView(
        _ tableView: NSTableView,
        viewFor tableColumn: NSTableColumn?,
        row: Int
    ) -> NSView? {
        let identifier = NSUserInterfaceItemIdentifier(tableView === vmTableView ? "vmCell" : "folderCell")
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
        if tableView === vmTableView {
            textField.stringValue = tableColumn?.identifier.rawValue == "createdAt"
                ? vmRecords[row].created_at
                : vmRecords[row].folder_path
            textField.lineBreakMode = .byTruncatingMiddle
        } else {
            textField.stringValue = folders[row]
        }
        return cell
    }

    private var cacheDirectoryURL: URL {
        let cacheDirectory = ProcessInfo.processInfo.environment["XDG_CACHE_HOME"]
            .map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".cache", isDirectory: true)
        return cacheDirectory.appendingPathComponent("vibe", isDirectory: true)
    }

    private var vmRegistryURL: URL {
        cacheDirectoryURL.appendingPathComponent("vm_registry.json")
    }

    private var mountedFoldersURL: URL {
        cacheDirectoryURL
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
