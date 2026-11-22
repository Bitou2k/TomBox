using UnrealBuildTool;

public class TomBoxLevelImport : ModuleRules
{
	public TomBoxLevelImport(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine" });
		PrivateDependencyModuleNames.AddRange(new string[]
		{
			"UnrealEd", "HTTP", "Json", "MeshDescription", "StaticMeshDescription", "AssetRegistry",
			"ImageWrapper", "MaterialEditor", "Slate", "SlateCore", "ToolMenus", "PropertyEditor",
			"EditorFramework", "LevelEditor"
		});
	}
}
