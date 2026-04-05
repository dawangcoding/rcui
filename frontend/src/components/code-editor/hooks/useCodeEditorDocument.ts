import { useCallback, useEffect, useState } from 'react';
import { api, authenticatedFetch } from '../../../utils/api';
import type { CodeEditorFile } from '../types/types';
import { isBinaryFile } from '../utils/binaryFile';

const IMAGE_EXTENSIONS = new Set([
  'png', 'jpg', 'jpeg', 'gif', 'svg', 'webp', 'ico', 'bmp',
]);

function isImageFile(filename: string): boolean {
  const ext = filename.split('.').pop()?.toLowerCase();
  return Boolean(ext && IMAGE_EXTENSIONS.has(ext));
}

type UseCodeEditorDocumentParams = {
  file: CodeEditorFile;
  projectPath?: string;
};

const getErrorMessage = (error: unknown) => {
  if (error instanceof Error) {
    return error.message;
  }

  return String(error);
};

export const useCodeEditorDocument = ({ file, projectPath }: UseCodeEditorDocumentParams) => {
  const [content, setContent] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [isBinary, setIsBinary] = useState(false);
  const [isImage, setIsImage] = useState(false);
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const fileProjectName = file.projectName ?? projectPath;
  const filePath = file.path;
  const fileName = file.name;
  const fileDiffNewString = file.diffInfo?.new_string;
  const fileDiffOldString = file.diffInfo?.old_string;

  useEffect(() => {
    let objectUrl: string | null = null;

    const loadFileContent = async () => {
      try {
        setLoading(true);
        setIsBinary(false);
        setIsImage(false);
        setImageUrl(null);

        // Check if file is binary by extension
        if (isBinaryFile(file.name)) {
          setIsBinary(true);
          setLoading(false);
          return;
        }

        // Check if file is an image — load via binary content endpoint
        if (isImageFile(file.name)) {
          setIsImage(true);
          // Use absolute-path endpoint for absolute paths (e.g. .tmp/images/),
          // otherwise fall back to project-scoped endpoint.
          const isAbsolute = filePath.startsWith('/');
          const contentUrl = isAbsolute
            ? `/api/files/raw?path=${encodeURIComponent(filePath)}`
            : `/api/projects/${fileProjectName}/files/content?path=${encodeURIComponent(filePath)}`;
          const response = await authenticatedFetch(contentUrl);
          if (response.ok) {
            const blob = await response.blob();
            objectUrl = URL.createObjectURL(blob);
            setImageUrl(objectUrl);
          }
          setLoading(false);
          return;
        }

        // Diff payload may already include full old/new snapshots, so avoid disk read.
        if (file.diffInfo && fileDiffNewString !== undefined && fileDiffOldString !== undefined) {
          setContent(fileDiffNewString);
          setLoading(false);
          return;
        }

        if (!fileProjectName) {
          throw new Error('Missing project identifier');
        }

        const response = await api.readFile(fileProjectName, filePath);
        if (!response.ok) {
          throw new Error(`Failed to load file: ${response.status} ${response.statusText}`);
        }

        const data = await response.json();
        setContent(data.content);
      } catch (error) {
        const message = getErrorMessage(error);
        console.error('Error loading file:', error);
        setContent(`// Error loading file: ${message}\n// File: ${fileName}\n// Path: ${filePath}`);
      } finally {
        setLoading(false);
      }
    };

    loadFileContent();

    return () => {
      if (objectUrl) {
        URL.revokeObjectURL(objectUrl);
      }
    };
  }, [file.diffInfo, file.name, fileDiffNewString, fileDiffOldString, fileName, filePath, fileProjectName]);

  const handleSave = useCallback(async () => {
    setSaving(true);
    setSaveError(null);

    try {
      if (!fileProjectName) {
        throw new Error('Missing project identifier');
      }

      const response = await api.saveFile(fileProjectName, filePath, content);

      if (!response.ok) {
        const contentType = response.headers.get('content-type');
        if (contentType?.includes('application/json')) {
          const errorData = await response.json();
          throw new Error(errorData.error || `Save failed: ${response.status}`);
        }

        const textError = await response.text();
        console.error('Non-JSON error response:', textError);
        throw new Error(`Save failed: ${response.status} ${response.statusText}`);
      }

      await response.json();

      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 2000);
    } catch (error) {
      const message = getErrorMessage(error);
      console.error('Error saving file:', error);
      setSaveError(message);
    } finally {
      setSaving(false);
    }
  }, [content, filePath, fileProjectName]);

  const handleDownload = useCallback(() => {
    const blob = new Blob([content], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');

    anchor.href = url;
    anchor.download = file.name;

    document.body.appendChild(anchor);
    anchor.click();
    document.body.removeChild(anchor);

    URL.revokeObjectURL(url);
  }, [content, file.name]);

  return {
    content,
    setContent,
    loading,
    saving,
    saveSuccess,
    saveError,
    isBinary,
    isImage,
    imageUrl,
    handleSave,
    handleDownload,
  };
};
