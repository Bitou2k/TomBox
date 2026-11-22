#pragma once

#include "CoreMinimal.h"
#include "UObject/Object.h"
#include "TomBoxImportOptions.generated.h"

/** What to import from a running TomBox map server (tools/rage/mapview/server.py). */
UCLASS(Config = EditorPerProjectUserSettings)
class TOMBOXLEVELIMPORT_API UTomBoxImportOptions : public UObject
{
	GENERATED_BODY()

public:
	/** Base URL of the map server (it listens on 127.0.0.1 only). */
	UPROPERTY(EditAnywhere, Config, Category = "Server")
	FString ServerUrl = TEXT("http://127.0.0.1:8765");

	/** Centre of the area, in game metres (X east, Y north, as the web viewer shows them). */
	UPROPERTY(EditAnywhere, Config, Category = "Area")
	FVector2D CenterMeters = FVector2D(-500.0, -800.0);

	/** Radius of the area in metres. Placements whose position lies inside are imported. */
	UPROPERTY(EditAnywhere, Config, Category = "Area", meta = (ClampMin = "10", ClampMax = "5000"))
	double RadiusMeters = 250.0;

	/** Detail level: 0 full detail, 1 "_lod" models, 2 "_slod" models (by model name). */
	UPROPERTY(EditAnywhere, Config, Category = "Area", meta = (ClampMin = "0", ClampMax = "2"))
	int32 DetailLevel = 0;

	/** Content folder for meshes, textures and materials (shared between imports). */
	UPROPERTY(EditAnywhere, Config, Category = "Output")
	FString ContentPath = TEXT("/Game/TomBox");

	/** Name of the map asset created in <ContentPath>/Maps. */
	UPROPERTY(EditAnywhere, Config, Category = "Output")
	FString MapName = TEXT("ImportedArea");

	/** Import textures (colour map of each part); off = grey materials only, much faster. */
	UPROPERTY(EditAnywhere, Config, Category = "Output")
	bool bImportTextures = true;

	/** Build meshes with Nanite enabled (slower import, cheaper rendering of dense areas). */
	UPROPERTY(EditAnywhere, Config, Category = "Output")
	bool bNanite = false;
};
