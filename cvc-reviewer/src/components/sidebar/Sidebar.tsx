import { useState } from "react";
import { motion } from "framer-motion";
import {
  File,
  FileCode,
  FilePlus,
  FileMinus,
  FileEdit,
  Check,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { clsx } from "clsx";

export interface FileItem {
  filename: string;
  status: "added" | "removed" | "modified" | "renamed" | "unchanged";
  additions: number;
  deletions: number;
  patch?: string;
}

interface SidebarProps {
  files: FileItem[];
  selectedFile: string | null;
  onSelectFile: (filename: string) => void;
  viewedFiles: Set<string>;
  onToggleViewed: (filename: string) => void;
}

// Group files by directory
function groupFilesByDirectory(files: FileItem[]): Map<string, FileItem[]> {
  const groups = new Map<string, FileItem[]>();

  for (const file of files) {
    const parts = file.filename.split("/");
    const dir = parts.length > 1 ? parts.slice(0, -1).join("/") : "";

    if (!groups.has(dir)) {
      groups.set(dir, []);
    }
    groups.get(dir)!.push(file);
  }

  return groups;
}

function getFileIcon(status: FileItem["status"]) {
  switch (status) {
    case "added":
      return <FilePlus size={14} className="text-success" />;
    case "removed":
      return <FileMinus size={14} className="text-danger" />;
    case "modified":
    case "renamed":
      return <FileEdit size={14} className="text-warning" />;
    default:
      return <File size={14} className="text-muted" />;
  }
}

function FileRow({
  file,
  isSelected,
  isViewed,
  onSelect,
  onToggleViewed,
}: {
  file: FileItem;
  isSelected: boolean;
  isViewed: boolean;
  onSelect: () => void;
  onToggleViewed: () => void;
}) {
  const fileName = file.filename.split("/").pop() || file.filename;

  return (
    <div
      className={clsx(
        "group flex cursor-pointer items-center gap-2 rounded-r-2xl px-3 py-2 transition-colors",
        isSelected
          ? "border-l-2 border-action bg-action/10"
          : "border-l-2 border-transparent hover:bg-canvas/70",
      )}
      onClick={onSelect}
    >
      <button
        onClick={(e) => {
          e.stopPropagation();
          onToggleViewed();
        }}
        className={clsx(
          "flex h-4 w-4 flex-shrink-0 items-center justify-center rounded border transition-colors",
          isViewed
            ? "border-success bg-success"
            : "border-muted/60 hover:border-action group-hover:border-action",
        )}
        aria-label={isViewed ? "Mark as unviewed" : "Mark as viewed"}
      >
        {isViewed && <Check size={10} className="text-white" />}
      </button>

      {getFileIcon(file.status)}

      <span
        className={clsx(
          "flex-1 truncate text-sm",
          isViewed ? "text-muted/70" : "text-ink",
        )}
        title={file.filename}
      >
        {fileName}
      </span>

      <div className="flex gap-1 text-xs font-mono opacity-0 group-hover:opacity-100 transition-opacity">
        {file.additions > 0 && (
          <span className="text-success">+{file.additions}</span>
        )}
        {file.deletions > 0 && (
          <span className="text-danger">-{file.deletions}</span>
        )}
      </div>
    </div>
  );
}

function DirectoryGroup({
  directory,
  files,
  selectedFile,
  viewedFiles,
  onSelectFile,
  onToggleViewed,
}: {
  directory: string;
  files: FileItem[];
  selectedFile: string | null;
  viewedFiles: Set<string>;
  onSelectFile: (filename: string) => void;
  onToggleViewed: (filename: string) => void;
}) {
  const [isExpanded, setIsExpanded] = useState(true);

  if (!directory) {
    // Root level files
    return (
      <>
        {files.map((file) => (
          <FileRow
            key={file.filename}
            file={file}
            isSelected={selectedFile === file.filename}
            isViewed={viewedFiles.has(file.filename)}
            onSelect={() => onSelectFile(file.filename)}
            onToggleViewed={() => onToggleViewed(file.filename)}
          />
        ))}
      </>
    );
  }

  return (
    <div>
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="flex w-full items-center gap-1 rounded-2xl px-3 py-2 text-muted transition-colors hover:bg-canvas/70 hover:text-ink"
      >
        {isExpanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <FileCode size={14} />
        <span className="text-sm truncate">{directory}</span>
        <span className="ml-auto text-xs text-muted">{files.length}</span>
      </button>

      {isExpanded && (
        <motion.div
          initial={{ height: 0, opacity: 0 }}
          animate={{ height: "auto", opacity: 1 }}
          exit={{ height: 0, opacity: 0 }}
          transition={{ duration: 0.1 }}
          className="pl-4"
        >
          {files.map((file) => (
            <FileRow
              key={file.filename}
              file={file}
              isSelected={selectedFile === file.filename}
              isViewed={viewedFiles.has(file.filename)}
              onSelect={() => onSelectFile(file.filename)}
              onToggleViewed={() => onToggleViewed(file.filename)}
            />
          ))}
        </motion.div>
      )}
    </div>
  );
}

export function Sidebar({
  files,
  selectedFile,
  onSelectFile,
  viewedFiles,
  onToggleViewed,
}: SidebarProps) {
  const groupedFiles = groupFilesByDirectory(files);
  const sortedDirs = Array.from(groupedFiles.keys()).sort();

  const viewedCount = files.filter((f) => viewedFiles.has(f.filename)).length;
  const totalCount = files.length;

  return (
    <div className="h-full flex flex-col">
      <div className="border-b border-line/80 px-4 py-4">
        <div className="flex items-center justify-between">
          <span className="font-serif text-lg font-semibold text-ink">Files</span>
          <span className="text-xs text-muted">
            {viewedCount}/{totalCount} viewed
          </span>
        </div>
        <div className="mt-3 h-1.5 overflow-hidden rounded-full bg-surface-strong/60">
          <div
            className="h-full bg-success transition-all duration-300"
            style={{
              width: `${totalCount > 0 ? (viewedCount / totalCount) * 100 : 0}%`,
            }}
          />
        </div>
      </div>

      <div className="flex-1 overflow-y-auto py-2">
        {sortedDirs.map((dir) => (
          <DirectoryGroup
            key={dir || "__root"}
            directory={dir}
            files={groupedFiles.get(dir)!}
            selectedFile={selectedFile}
            viewedFiles={viewedFiles}
            onSelectFile={onSelectFile}
            onToggleViewed={onToggleViewed}
          />
        ))}

        {files.length === 0 && (
          <div className="px-4 py-8 text-center text-sm text-muted">
            No files in this PR
          </div>
        )}
      </div>
    </div>
  );
}
