type VersionInfoSectionProps = {
  currentVersion: string;
};

export default function VersionInfoSection({
  currentVersion,
}: VersionInfoSectionProps) {
  return (
    <div className="border-t border-border/50 pt-6">
      <div className="flex items-center justify-between text-xs italic text-muted-foreground/60">
        <span>v{currentVersion}</span>
      </div>
    </div>
  );
}
