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
      return <FilePlus size={14} className="text-[#27a300]" />;
    case "removed":
      return <FileMinus size={14} className="text-[#ef4444]" />;
    case "modified":
    case "renamed":
      return <FileEdit size={14} className="text-[#f59e0b]" />;
    default:
      return <File size={14} className="text-[#888888]" />;
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
        "group flex items-center gap-2 px-3 py-1.5 cursor-pointer transition-colors",
        isSelected
          ? "bg-[#5e6ad2]/20 border-l-2 border-[#5e6ad2]"
          : "hover:bg-[#1c1c1c] border-l-2 border-transparent",
      )}
      onClick={onSelect}
    >
      {/* Viewed checkbox */}
      <button
        onClick={(e) => {
          e.stopPropagation();
          onToggleViewed();
        }}
        className={clsx(
          "w-4 h-4 rounded border flex items-center justify-center transition-colors flex-shrink-0",
          isViewed
            ? "bg-[#27a300] border-[#27a300]"
            : "border-[#555555] hover:border-[#888888] group-hover:border-[#888888]",
        )}
        aria-label={isViewed ? "Mark as unviewed" : "Mark as viewed"}
      >
        {isViewed && <Check size={10} className="text-white" />}
      </button>

      {/* File icon */}
      {getFileIcon(file.status)}

      {/* File name */}
      <span
        className={clsx(
          "flex-1 truncate text-sm",
          isViewed ? "text-[#555555]" : "text-[#ededed]",
        )}
        title={file.filename}
      >
        {fileName}
      </span>

      {/* Change stats */}
      <div className="flex gap-1 text-xs font-mono opacity-0 group-hover:opacity-100 transition-opacity">
        {file.additions > 0 && (
          <span className="text-[#27a300]">+{file.additions}</span>
        )}
        {file.deletions > 0 && (
          <span className="text-[#ef4444]">-{file.deletions}</span>
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
        className="w-full flex items-center gap-1 px-3 py-1.5 text-[#888888] hover:text-[#ededed] hover:bg-[#1c1c1c] transition-colors"
      >
        {isExpanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <FileCode size={14} />
        <span className="text-sm truncate">{directory}</span>
        <span className="text-xs text-[#555555] ml-auto">{files.length}</span>
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
      {/* Header */}
      <div className="px-4 py-3 border-b border-[#1c1c1c]">
        <div className="flex items-center justify-between">
          <span className="text-sm font-medium text-[#ededed]">Files</span>
          <span className="text-xs text-[#888888]">
            {viewedCount}/{totalCount} viewed
          </span>
        </div>
        {/* Progress bar */}
        <div className="mt-2 h-1 bg-[#1c1c1c] rounded-full overflow-hidden">
          <div
            className="h-full bg-[#27a300] transition-all duration-300"
            style={{
              width: `${totalCount > 0 ? (viewedCount / totalCount) * 100 : 0}%`,
            }}
          />
        </div>
      </div>

      {/* File list */}
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
          <div className="px-4 py-8 text-center text-[#555555] text-sm">
            No files in this PR
          </div>
        )}
      </div>
    </div>
  );
}
